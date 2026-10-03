use super::process::UnifiedExecProcess;
use crate::unified_exec::UnifiedExecError;
use codex_exec_server::ExecProcess;
use codex_exec_server::ExecProcessEventReceiver;
use codex_exec_server::ExecProcessFuture;
use codex_exec_server::ExecServerError;
use codex_exec_server::ProcessId;
use codex_exec_server::ProcessSignal;
use codex_exec_server::ReadResponse;
use codex_exec_server::StartedExecProcess;
use codex_exec_server::WriteResponse;
use codex_exec_server::WriteStatus;
use pretty_assertions::assert_eq;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::watch;

struct MockExecProcess {
    process_id: ProcessId,
    write_response: WriteResponse,
    read_responses: Mutex<VecDeque<ReadResponse>>,
    terminate_error: Option<String>,
    terminate_calls: Arc<AtomicUsize>,
    wake_tx: watch::Sender<u64>,
}

impl MockExecProcess {
    async fn read(&self) -> Result<ReadResponse, ExecServerError> {
        Ok(self
            .read_responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(ReadResponse {
                chunks: Vec::new(),
                next_seq: 1,
                exited: false,
                exit_code: None,
                closed: false,
                failure: None,
                sandbox_denied: false,
            }))
    }

    async fn terminate(&self) -> Result<(), ExecServerError> {
        self.terminate_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = &self.terminate_error {
            return Err(ExecServerError::Protocol(message.clone()));
        }
        Ok(())
    }
}

impl ExecProcess for MockExecProcess {
    fn process_id(&self) -> &ProcessId {
        &self.process_id
    }

    fn subscribe_wake(&self) -> watch::Receiver<u64> {
        self.wake_tx.subscribe()
    }

    fn subscribe_events(&self) -> ExecProcessEventReceiver {
        ExecProcessEventReceiver::empty()
    }

    fn read(
        &self,
        _after_seq: Option<u64>,
        _max_bytes: Option<usize>,
        _wait_ms: Option<u64>,
    ) -> ExecProcessFuture<'_, ReadResponse> {
        Box::pin(MockExecProcess::read(self))
    }

    fn write(&self, _chunk: Vec<u8>) -> ExecProcessFuture<'_, WriteResponse> {
        Box::pin(async { Ok(self.write_response.clone()) })
    }

    fn signal(&self, _signal: ProcessSignal) -> ExecProcessFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn terminate(&self) -> ExecProcessFuture<'_, ()> {
        Box::pin(MockExecProcess::terminate(self))
    }
}

pub(super) async fn remote_process(
    write_status: WriteStatus,
    terminate_error: Option<String>,
    sandbox_type: impl Into<Option<codex_sandboxing::SandboxType>>,
) -> UnifiedExecProcess {
    tracked_remote_process(write_status, terminate_error, sandbox_type)
        .await
        .0
}

async fn tracked_remote_process(
    write_status: WriteStatus,
    terminate_error: Option<String>,
    sandbox_type: impl Into<Option<codex_sandboxing::SandboxType>>,
) -> (UnifiedExecProcess, Arc<AtomicUsize>) {
    let terminate_calls = Arc::new(AtomicUsize::new(0));
    let (wake_tx, _wake_rx) = watch::channel(0);
    let started = StartedExecProcess {
        process: Arc::new(MockExecProcess {
            process_id: "test-process".to_string().into(),
            write_response: WriteResponse {
                status: write_status,
            },
            read_responses: Mutex::new(VecDeque::new()),
            terminate_error,
            terminate_calls: Arc::clone(&terminate_calls),
            wake_tx,
        }),
        sandbox_type: sandbox_type.into(),
    };

    let process = UnifiedExecProcess::from_exec_server_started(started)
        .await
        .expect("remote process should start");
    (process, terminate_calls)
}

#[tokio::test]
async fn remote_write_unknown_process_marks_process_exited() {
    let process = remote_process(
        WriteStatus::UnknownProcess,
        /*terminate_error*/ None,
        codex_sandboxing::SandboxType::None,
    )
    .await;

    let err = process
        .write(b"hello")
        .await
        .expect_err("expected write failure");

    assert!(matches!(err, UnifiedExecError::WriteToStdin));
    assert!(process.has_exited());
}

#[tokio::test]
async fn remote_write_closed_stdin_marks_process_exited() {
    let process = remote_process(
        WriteStatus::StdinClosed,
        /*terminate_error*/ None,
        codex_sandboxing::SandboxType::None,
    )
    .await;

    let err = process
        .write(b"hello")
        .await
        .expect_err("expected write failure");

    assert!(matches!(err, UnifiedExecError::WriteToStdin));
    assert!(process.has_exited());
}

#[tokio::test]
async fn fail_and_terminate_preserves_failure_message() {
    let process = remote_process(
        WriteStatus::Accepted,
        /*terminate_error*/ None,
        codex_sandboxing::SandboxType::None,
    )
    .await;

    process.fail_and_terminate("network denied".to_string());
    process.fail_and_terminate("second failure".to_string());

    assert!(process.has_exited());
    assert_eq!(
        process.failure_message(),
        Some("network denied".to_string())
    );
}

#[tokio::test]
async fn remote_terminate_confirmed_updates_state_on_success_only() {
    let process = remote_process(
        WriteStatus::Accepted,
        Some("terminate unavailable".to_string()),
        codex_sandboxing::SandboxType::None,
    )
    .await;

    let err = process
        .terminate_confirmed()
        .await
        .expect_err("expected terminate failure");

    assert!(matches!(err, UnifiedExecError::ProcessFailed { .. }));
    assert!(!process.has_exited());

    let process = remote_process(
        WriteStatus::Accepted,
        /*terminate_error*/ None,
        codex_sandboxing::SandboxType::None,
    )
    .await;

    process
        .terminate_confirmed()
        .await
        .expect("terminate should succeed");

    assert!(process.has_exited());
}

#[tokio::test]
async fn remote_process_preserves_executor_sandbox_type() {
    use codex_sandboxing::SandboxType;
    for sandbox_type in [
        None,
        Some(SandboxType::None),
        Some(SandboxType::LinuxSeccomp),
    ] {
        let process = remote_process(
            WriteStatus::Accepted,
            /*terminate_error*/ None,
            sandbox_type,
        )
        .await;
        assert_eq!(process.sandbox_type(), sandbox_type);
    }
}

#[tokio::test(start_paused = true)]
async fn runtime_deadline_terminates_unpolled_process() {
    let (process, terminate_calls) =
        tracked_remote_process(WriteStatus::Accepted, None, None).await;
    let process = Arc::new(process);
    // Dropping the task handle models a caller that never polls its yielded session.
    drop(process.enforce_timeout(Duration::from_secs(60)));
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(!process.has_exited());
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 0);

    tokio::time::advance(Duration::from_secs(1)).await;
    process.cancellation_token().cancelled().await;
    assert!(process.has_exited());
    assert!(process.timed_out());
    assert_eq!(process.exit_code(), Some(124));
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn runtime_deadline_preserves_an_earlier_exit() {
    let (process, terminate_calls) =
        tracked_remote_process(WriteStatus::Accepted, None, None).await;
    let process = Arc::new(process);
    let deadline = process.enforce_timeout(Duration::from_secs(60));
    process.terminate_confirmed().await.unwrap();
    let exit_code = process.exit_code();
    tokio::time::advance(Duration::from_secs(61)).await;
    deadline.await.unwrap();

    assert!(!process.timed_out());
    assert_eq!(process.exit_code(), exit_code);
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn runtime_deadline_does_not_retain_a_dropped_process() {
    let process = Arc::new(remote_process(WriteStatus::Accepted, None, None).await);
    let weak = Arc::downgrade(&process);
    let deadline = process.enforce_timeout(Duration::from_secs(60));
    tokio::task::yield_now().await;
    drop(process);

    assert!(weak.upgrade().is_none());
    deadline.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn runtime_deadline_reports_termination_failure() {
    let process = Arc::new(
        remote_process(
            WriteStatus::Accepted,
            Some("terminate unavailable".to_string()),
            None,
        )
        .await,
    );
    let deadline = process.enforce_timeout(Duration::from_secs(60));
    tokio::time::advance(Duration::from_secs(60)).await;
    deadline.await.unwrap();

    assert!(process.timed_out());
    assert!(process.has_exited());
    assert!(
        process
            .failure_message()
            .is_some_and(|message| message.contains("terminate unavailable"))
    );
}
