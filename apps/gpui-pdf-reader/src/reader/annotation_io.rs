use crate::annotations::{
    AnnotationSet, AnnotationStore, DocumentIdentity, DocumentKey, JsonSidecarStore,
};
use std::{collections::HashMap, path::PathBuf, sync::mpsc, thread};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AnnotationIoOperation {
    Load,
    Save,
}

enum AnnotationIoCommand {
    Load {
        generation: u64,
        path: PathBuf,
        page_count: usize,
    },
    Save {
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    },
}

pub(super) enum AnnotationIoEvent {
    Loaded {
        generation: u64,
        identity: DocumentIdentity,
        annotations: AnnotationSet,
    },
    Saved {
        generation: u64,
        revision: u64,
    },
    Failed {
        generation: u64,
        operation: AnnotationIoOperation,
        revision: Option<u64>,
        message: String,
    },
}

pub(super) struct AnnotationIo {
    commands: Option<mpsc::Sender<AnnotationIoCommand>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl AnnotationIo {
    pub(super) fn start() -> (Self, mpsc::Receiver<AnnotationIoEvent>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("annotation-sidecar".into())
            .spawn(move || {
                let mut deferred = None;
                let mut observed_disk_revisions = HashMap::<(u64, PathBuf), u64>::new();
                loop {
                    let command = match deferred.take() {
                        Some(command) => command,
                        None => match command_rx.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };
                    match command {
                        AnnotationIoCommand::Load {
                            generation,
                            path,
                            page_count,
                        } => match DocumentKey::from_pdf(path.clone(), page_count).and_then(|key| {
                            JsonSidecarStore
                                .load(&key)
                                .map(|annotations| (key.identity().clone(), annotations))
                        }) {
                            Ok((identity, annotations)) => {
                                // A load is an ordering boundary for the sole
                                // current document; older generations can no
                                // longer save and must not accumulate paths.
                                observed_disk_revisions.clear();
                                observed_disk_revisions
                                    .insert((generation, path.clone()), annotations.revision());
                                let _ = event_tx.send(AnnotationIoEvent::Loaded {
                                    generation,
                                    identity,
                                    annotations,
                                });
                            }
                            Err(error) => {
                                observed_disk_revisions.clear();
                                let _ = event_tx.send(AnnotationIoEvent::Failed {
                                    generation,
                                    operation: AnnotationIoOperation::Load,
                                    revision: None,
                                    message: error.to_string(),
                                });
                            }
                        },
                        AnnotationIoCommand::Save {
                            generation,
                            path,
                            mut identity,
                            expected_disk_revision,
                            mut annotations,
                        } => {
                            // Saving a sidecar snapshot is much slower than a
                            // color click. Collapse a queued same-document
                            // burst to its newest revision while preserving a
                            // load or another document's ordering boundary. The
                            // first command's expected disk revision remains the
                            // base for the whole coalesced burst.
                            while let Ok(command) = command_rx.try_recv() {
                                match command {
                                    AnnotationIoCommand::Save {
                                        generation: next_generation,
                                        path: next_path,
                                        identity: next_identity,
                                        expected_disk_revision: _,
                                        annotations: next_annotations,
                                    } if next_generation == generation && next_path == path => {
                                        identity = next_identity;
                                        annotations = next_annotations;
                                    }
                                    command => {
                                        deferred = Some(command);
                                        break;
                                    }
                                }
                            }
                            let revision = annotations.revision();
                            let revision_key = (generation, path.clone());
                            let expected_disk_revision = observed_disk_revisions
                                .get(&revision_key)
                                .copied()
                                .unwrap_or(expected_disk_revision);
                            let document = DocumentKey::new(path.clone(), identity);
                            let save_result = JsonSidecarStore.compare_and_save(
                                &document,
                                expected_disk_revision,
                                &annotations,
                            );
                            match save_result {
                                Ok(receipt) => {
                                    observed_disk_revisions
                                        .insert(revision_key, receipt.saved_revision);
                                    let _ = event_tx.send(AnnotationIoEvent::Saved {
                                        generation,
                                        revision: receipt.saved_revision,
                                    });
                                }
                                Err(error) => {
                                    let _ = event_tx.send(AnnotationIoEvent::Failed {
                                        generation,
                                        operation: AnnotationIoOperation::Save,
                                        revision: Some(revision),
                                        message: error.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            })
            .expect("failed to start the annotation sidecar thread");
        (
            Self {
                commands: Some(command_tx),
                thread: Some(thread),
            },
            event_rx,
        )
    }

    pub(super) fn load(&self, generation: u64, path: PathBuf, page_count: usize) -> bool {
        self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(AnnotationIoCommand::Load {
                    generation,
                    path,
                    page_count,
                })
                .is_ok()
        })
    }

    pub(super) fn save(
        &self,
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    ) -> bool {
        self.commands.as_ref().is_some_and(|commands| {
            commands
                .send(AnnotationIoCommand::Save {
                    generation,
                    path,
                    identity,
                    expected_disk_revision,
                    annotations,
                })
                .is_ok()
        })
    }
}

impl Drop for AnnotationIo {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
