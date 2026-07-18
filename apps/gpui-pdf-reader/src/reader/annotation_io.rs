use crate::annotations::{AnnotationSet, DocumentIdentity};
use key_sidecar_store::{
    AnnotationService, AnnotationServiceClient, AnnotationServiceEvent, AnnotationServiceEventKind,
    AnnotationServiceOperation,
};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::mpsc;
#[cfg(test)]
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AnnotationIoOperation {
    Load,
    Save,
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

/// Compatibility receiver that keeps the reader-facing event contract narrow
/// while the shared service carries client and document routing metadata.
pub(super) struct AnnotationIoEvents(mpsc::Receiver<AnnotationServiceEvent>);

impl AnnotationIoEvents {
    pub(super) fn recv(&self) -> Result<AnnotationIoEvent, mpsc::RecvError> {
        self.0.recv().map(map_event)
    }

    #[cfg(test)]
    pub(super) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<AnnotationIoEvent, mpsc::RecvTimeoutError> {
        self.0.recv_timeout(timeout).map(map_event)
    }
}

fn map_event(event: AnnotationServiceEvent) -> AnnotationIoEvent {
    match event.kind {
        AnnotationServiceEventKind::Loaded {
            identity,
            annotations,
        } => AnnotationIoEvent::Loaded {
            generation: event.generation,
            identity,
            annotations,
        },
        AnnotationServiceEventKind::Saved { revision } => AnnotationIoEvent::Saved {
            generation: event.generation,
            revision,
        },
        AnnotationServiceEventKind::Failed {
            operation,
            revision,
            message,
        } => AnnotationIoEvent::Failed {
            generation: event.generation,
            operation: match operation {
                AnnotationServiceOperation::Load => AnnotationIoOperation::Load,
                AnnotationServiceOperation::Save => AnnotationIoOperation::Save,
            },
            revision,
            message,
        },
    }
}

#[cfg(test)]
fn process_annotation_service() -> &'static AnnotationService {
    static SERVICE: OnceLock<AnnotationService> = OnceLock::new();
    SERVICE.get_or_init(AnnotationService::start)
}

/// Per-reader adapter over the process-level annotation service.
pub(super) struct AnnotationIo {
    client: AnnotationServiceClient,
}

impl AnnotationIo {
    pub(super) fn attach(service: &AnnotationService) -> (Self, AnnotationIoEvents) {
        let (client, events) = service
            .attach()
            .expect("the process annotation service must remain available");
        (Self { client }, AnnotationIoEvents(events))
    }

    #[cfg(test)]
    pub(super) fn start() -> (Self, AnnotationIoEvents) {
        Self::attach(process_annotation_service())
    }

    pub(super) fn load(&self, generation: u64, path: PathBuf, page_count: usize) -> bool {
        self.client.load(generation, path, page_count)
    }

    pub(super) fn save(
        &self,
        generation: u64,
        path: PathBuf,
        identity: DocumentIdentity,
        expected_disk_revision: u64,
        annotations: AnnotationSet,
    ) -> bool {
        self.client.save(
            generation,
            path,
            identity,
            expected_disk_revision,
            annotations,
        )
    }
}
