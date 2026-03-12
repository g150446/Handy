use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use log::{debug, error, warn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const DEBOUNCE: Duration = Duration::from_millis(30);

/// Commands processed sequentially by the coordinator thread.
enum Command {
    Input {
        binding_id: String,
        hotkey_string: String,
        is_pressed: bool,
        push_to_talk: bool,
    },
    Cancel {
        recording_was_active: bool,
    },
    ProcessingFinished {
        epoch: u64,
    },
}

/// Pipeline lifecycle, owned exclusively by the coordinator thread.
enum Stage {
    Idle,
    Recording(String), // binding_id
    Processing { epoch: u64 },
}

fn stage_label(stage: &Stage) -> String {
    match stage {
        Stage::Idle => "Idle".to_string(),
        Stage::Recording(binding_id) => format!("Recording({binding_id})"),
        Stage::Processing { epoch } => format!("Processing(epoch={epoch})"),
    }
}

fn apply_lifecycle_command(stage: &mut Stage, command: &Command) {
    match command {
        Command::Cancel {
            recording_was_active,
        } => {
            let previous = stage_label(stage);
            if *recording_was_active
                || matches!(stage, Stage::Recording(_) | Stage::Processing { .. })
            {
                *stage = Stage::Idle;
                debug!(
                    "Coordinator cancel reset stage from {} to {} (recording_was_active={})",
                    previous,
                    stage_label(stage),
                    recording_was_active
                );
            } else {
                debug!(
                    "Coordinator cancel left stage unchanged at {} (recording_was_active={})",
                    previous, recording_was_active
                );
            }
        }
        Command::ProcessingFinished { epoch } => {
            let previous = stage_label(stage);
            if matches!(stage, Stage::Processing { epoch: active_epoch } if *active_epoch == *epoch)
            {
                *stage = Stage::Idle;
                debug!(
                    "Coordinator processing finished for epoch {}: {} -> {}",
                    epoch,
                    previous,
                    stage_label(stage)
                );
            } else {
                debug!(
                    "Ignoring processing-finished for epoch {} while stage is {}",
                    epoch, previous
                );
            }
        }
        Command::Input { .. } => {}
    }
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline.
pub struct TranscriptionCoordinator {
    tx: Sender<Command>,
    processing_epoch: Arc<AtomicU64>,
}

pub fn is_transcribe_binding(id: &str) -> bool {
    id == "transcribe" || id == "transcribe_with_post_process"
}

impl TranscriptionCoordinator {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        let processing_epoch = Arc::new(AtomicU64::new(0));

        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut stage = Stage::Idle;
                let mut last_press: Option<Instant> = None;

                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Command::Input {
                            binding_id,
                            hotkey_string,
                            is_pressed,
                            push_to_talk,
                        } => {
                            // Debounce rapid-fire press events (key repeat / double-tap).
                            // Releases always pass through for push-to-talk.
                            if is_pressed {
                                let now = Instant::now();
                                if last_press.map_or(false, |t| now.duration_since(t) < DEBOUNCE) {
                                    debug!(
                                        "Debounced press for '{}' while stage={}",
                                        binding_id,
                                        stage_label(&stage)
                                    );
                                    continue;
                                }
                                last_press = Some(now);
                            }

                            debug!(
                                "Coordinator input binding='{}' pressed={} push_to_talk={} stage={}",
                                binding_id,
                                is_pressed,
                                push_to_talk,
                                stage_label(&stage)
                            );

                            if push_to_talk {
                                if is_pressed && matches!(stage, Stage::Idle) {
                                    start(&app, &mut stage, &binding_id, &hotkey_string);
                                } else if !is_pressed
                                    && matches!(&stage, Stage::Recording(id) if id == &binding_id)
                                {
                                    stop(&app, &mut stage, &binding_id, &hotkey_string);
                                }
                            } else if is_pressed {
                                match &stage {
                                    Stage::Idle => {
                                        start(&app, &mut stage, &binding_id, &hotkey_string);
                                    }
                                    Stage::Recording(id) if id == &binding_id => {
                                        stop(&app, &mut stage, &binding_id, &hotkey_string);
                                    }
                                    _ => {
                                        debug!(
                                            "Ignoring press for '{}' because coordinator stage={}",
                                            binding_id,
                                            stage_label(&stage)
                                        )
                                    }
                                }
                            }
                        }
                        Command::Cancel { .. } | Command::ProcessingFinished { .. } => {
                            apply_lifecycle_command(&mut stage, &cmd);
                        }
                    }
                }
                debug!("Transcription coordinator exited");
            }));
            if let Err(e) = result {
                error!("Transcription coordinator panicked: {e:?}");
            }
        });

        Self {
            tx,
            processing_epoch,
        }
    }

    /// Send a keyboard/signal input event for a transcribe binding.
    /// For signal-based toggles, use `is_pressed: true` and `push_to_talk: false`.
    pub fn send_input(
        &self,
        binding_id: &str,
        hotkey_string: &str,
        is_pressed: bool,
        push_to_talk: bool,
    ) {
        if self
            .tx
            .send(Command::Input {
                binding_id: binding_id.to_string(),
                hotkey_string: hotkey_string.to_string(),
                is_pressed,
                push_to_talk,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        if self
            .tx
            .send(Command::Cancel {
                recording_was_active,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn current_processing_epoch(&self) -> u64 {
        self.processing_epoch.load(Ordering::SeqCst)
    }

    pub fn advance_processing_epoch(&self) -> u64 {
        self.processing_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn notify_processing_finished(&self, epoch: u64) {
        if self.tx.send(Command::ProcessingFinished { epoch }).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }
}

fn start(app: &AppHandle, stage: &mut Stage, binding_id: &str, hotkey_string: &str) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.start(app, binding_id, hotkey_string);
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .map_or(false, |a| a.is_recording())
    {
        *stage = Stage::Recording(binding_id.to_string());
        debug!(
            "Coordinator start accepted for '{}' -> {}",
            binding_id,
            stage_label(stage)
        );
    } else {
        debug!("Start for '{binding_id}' did not begin recording; staying idle");
    }
}

fn stop(app: &AppHandle, stage: &mut Stage, binding_id: &str, hotkey_string: &str) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    let epoch = app
        .try_state::<TranscriptionCoordinator>()
        .map(|coordinator| coordinator.current_processing_epoch())
        .unwrap_or(0);
    action.stop(app, binding_id, hotkey_string);
    *stage = Stage::Processing { epoch };
    debug!(
        "Coordinator stop accepted for '{}' -> {}",
        binding_id,
        stage_label(stage)
    );
}

#[cfg(test)]
mod tests {
    use super::{apply_lifecycle_command, Command, Stage};

    #[test]
    fn cancel_during_processing_returns_to_idle_immediately() {
        let mut stage = Stage::Processing { epoch: 3 };

        apply_lifecycle_command(
            &mut stage,
            &Command::Cancel {
                recording_was_active: false,
            },
        );

        assert!(matches!(stage, Stage::Idle));
    }

    #[test]
    fn stale_processing_finished_does_not_reset_newer_processing_stage() {
        let mut stage = Stage::Processing { epoch: 4 };

        apply_lifecycle_command(&mut stage, &Command::ProcessingFinished { epoch: 3 });

        assert!(matches!(stage, Stage::Processing { epoch: 4 }));
    }

    #[test]
    fn matching_processing_finished_resets_processing_stage() {
        let mut stage = Stage::Processing { epoch: 5 };

        apply_lifecycle_command(&mut stage, &Command::ProcessingFinished { epoch: 5 });

        assert!(matches!(stage, Stage::Idle));
    }
}
