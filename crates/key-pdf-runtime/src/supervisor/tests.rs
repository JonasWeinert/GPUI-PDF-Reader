use super::*;
use crate::{ColorMode, DemandIntent, TextDemandPurpose};
use key_pdf_core::{PageSize, PixelRect, RasterSize, TextLayer, TileKey};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
struct MockSource(&'static str);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MockError;

impl fmt::Display for MockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("mock PDF error")
    }
}

impl std::error::Error for MockError {}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    Open(&'static str),
    Render(&'static str, u32),
    Text(&'static str, usize),
    Preview(&'static str, usize),
}

#[derive(Default)]
struct GateState {
    entered: usize,
    released: bool,
}

#[derive(Default)]
struct Probe {
    owner: Mutex<Option<ThreadId>>,
    calls: Mutex<Vec<Call>>,
    block_next_render: AtomicBool,
    gate: (Mutex<GateState>, Condvar),
}

impl Probe {
    fn assert_owner(&self) {
        let current = thread::current().id();
        let mut owner = self.owner.lock().unwrap();
        match *owner {
            Some(owner) => assert_eq!(owner, current, "engine escaped its owner thread"),
            None => *owner = Some(current),
        }
    }

    fn record(&self, call: Call) {
        self.assert_owner();
        self.calls.lock().unwrap().push(call);
    }

    fn block_one_render(&self) {
        let (state, _) = &self.gate;
        let mut state = state.lock().unwrap();
        state.released = false;
        self.block_next_render.store(true, Ordering::Release);
    }

    fn wait_until_render_entered(&self) {
        let deadline = Instant::now() + TIMEOUT;
        let (state, changed) = &self.gate;
        let mut state = state.lock().unwrap();
        while state.entered == 0 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("timed out waiting for blocked render");
            let result = changed.wait_timeout(state, remaining).unwrap();
            state = result.0;
            assert!(
                !result.1.timed_out(),
                "timed out waiting for blocked render"
            );
        }
    }

    fn release_render(&self) {
        let (state, changed) = &self.gate;
        state.lock().unwrap().released = true;
        changed.notify_all();
    }

    fn maybe_block_render(&self) {
        if !self.block_next_render.swap(false, Ordering::AcqRel) {
            return;
        }
        let (state, changed) = &self.gate;
        let mut state = state.lock().unwrap();
        state.entered += 1;
        changed.notify_all();
        while !state.released {
            state = changed.wait(state).unwrap();
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

struct MockEngine {
    probe: Arc<Probe>,
    not_send: std::rc::Rc<()>,
}

struct MockDocument {
    name: &'static str,
    probe: Arc<Probe>,
    descriptor: DocumentDescriptor,
    not_send: std::rc::Rc<()>,
}

impl PdfEngine for MockEngine {
    type Source = MockSource;
    type Document = MockDocument;
    type Error = MockError;

    fn capabilities(&self) -> EngineCapabilities {
        let _ = &self.not_send;
        self.probe.assert_owner();
        EngineCapabilities {
            text: true,
            previews: true,
            ..EngineCapabilities::default()
        }
    }

    fn open(
        &mut self,
        source: Self::Source,
        cancellation: &CancellationToken,
    ) -> Result<Self::Document, Self::Error> {
        self.probe.record(Call::Open(source.0));
        cancellation.checkpoint().map_err(|_| MockError)?;
        Ok(MockDocument {
            name: source.0,
            probe: self.probe.clone(),
            descriptor: DocumentDescriptor::new(
                vec![PageSize {
                    width: 612.0,
                    height: 792.0,
                }],
                Vec::new(),
                Vec::new(),
            ),
            not_send: std::rc::Rc::new(()),
        })
    }
}

impl EngineDocument for MockDocument {
    type Error = MockError;

    fn descriptor(&self) -> &DocumentDescriptor {
        &self.descriptor
    }

    fn render(
        &mut self,
        demand: &RenderDemand,
        cancellation: &CancellationToken,
    ) -> Result<crate::RasterImage, Self::Error> {
        let _ = &self.not_send;
        self.probe
            .record(Call::Render(self.name, demand.key().column));
        self.probe.maybe_block_render();
        cancellation.checkpoint().map_err(|_| MockError)?;
        image(demand.render_rect().width, demand.render_rect().height)
    }

    fn extract_text(
        &mut self,
        demand: &TextDemand,
        cancellation: &CancellationToken,
    ) -> Result<TextLayer, Self::Error> {
        self.probe.record(Call::Text(self.name, demand.page()));
        cancellation.checkpoint().map_err(|_| MockError)?;
        Ok(TextLayer::empty())
    }

    fn render_preview(
        &mut self,
        demand: &PreviewDemand,
        cancellation: &CancellationToken,
    ) -> Result<crate::RasterImage, Self::Error> {
        self.probe.record(Call::Preview(self.name, demand.page()));
        cancellation.checkpoint().map_err(|_| MockError)?;
        image(demand.region().width, demand.region().height)
    }
}

fn image(width: u32, height: u32) -> Result<crate::RasterImage, MockError> {
    let stride = width as usize * 4;
    crate::RasterImage::new(
        width,
        height,
        stride,
        crate::PixelFormat::Bgra8Premultiplied,
        vec![0; stride * height as usize],
    )
    .map_err(|_| MockError)
}

type TestSupervisor = EngineSupervisor<MockSource, MockError>;
type TestClient = DocumentClient<MockSource, MockError>;

fn supervisor(probe: Arc<Probe>, policy: SupervisorPolicy) -> TestSupervisor {
    start_engine_supervisor("mock-pdf-owner", policy, move || MockEngine {
        probe,
        not_send: std::rc::Rc::new(()),
    })
    .unwrap()
}

fn attach_open(
    supervisor: &TestSupervisor,
    name: &'static str,
) -> (
    TestClient,
    mpsc::Receiver<SupervisorEvent<MockError>>,
    DocumentSession,
) {
    let (client, events) = supervisor.attach().unwrap();
    assert!(matches!(
        events.recv_timeout(TIMEOUT).unwrap(),
        SupervisorEvent::Attached { document, .. } if document == client.id()
    ));
    client.open(MockSource(name)).unwrap();
    let session = match events.recv_timeout(TIMEOUT).unwrap() {
        SupervisorEvent::Opened {
            document,
            session,
            descriptor,
            ..
        } => {
            assert_eq!(document, client.id());
            assert_eq!(descriptor.page_count(), 1);
            session
        }
        other => panic!("expected opened event, received {other:?}"),
    };
    (client, events, session)
}

fn render(session: &DocumentSession, column: u32, priority: DemandPriority) -> RenderDemand {
    session
        .render_demand(
            TileKey {
                page: 0,
                raster: RasterSize {
                    width: 16,
                    height: 16,
                },
                column,
                row: 0,
            },
            PixelRect {
                x: 0,
                y: 0,
                width: 16,
                height: 16,
            },
            PixelRect {
                x: 0,
                y: 0,
                width: 16,
                height: 16,
            },
            ColorMode::Original,
            priority,
            DemandIntent::Visible,
        )
        .unwrap()
}

fn expect_render_ready(events: &mpsc::Receiver<SupervisorEvent<MockError>>, column: u32) {
    match events.recv_timeout(TIMEOUT).unwrap() {
        SupervisorEvent::Rendered {
            event: RenderEvent::Ready { demand, .. },
            ..
        } => assert_eq!(demand.key().column, column),
        other => panic!("expected ready render {column}, received {other:?}"),
    }
}

#[test]
fn one_owner_thread_handles_multiple_non_send_documents_and_operation_types() {
    let probe = Arc::new(Probe::default());
    let supervisor = supervisor(probe.clone(), SupervisorPolicy::default());
    let (alpha, alpha_events, alpha_session) = attach_open(&supervisor, "alpha");
    let (beta, beta_events, beta_session) = attach_open(&supervisor, "beta");

    alpha
        .replace_render_viewport(vec![render(&alpha_session, 1, DemandPriority::VISIBLE)])
        .unwrap();
    beta.replace_text(
        WorkClass::CopyText,
        vec![
            beta_session
                .text_demand(
                    0,
                    TextDemandPurpose::Copy,
                    DemandPriority::INTERACTIVE,
                    DemandIntent::Explicit,
                )
                .unwrap(),
        ],
    )
    .unwrap();
    expect_render_ready(&alpha_events, 1);
    assert!(matches!(
        beta_events.recv_timeout(TIMEOUT).unwrap(),
        SupervisorEvent::TextExtracted {
            event: TextEvent::Ready { .. },
            ..
        }
    ));

    let owner = probe.owner.lock().unwrap().expect("owner was recorded");
    assert_ne!(owner, thread::current().id());
    assert_eq!(
        probe.calls(),
        vec![
            Call::Open("alpha"),
            Call::Open("beta"),
            Call::Render("alpha", 1),
            Call::Text("beta", 0),
        ]
    );
}

#[test]
fn round_robin_fairness_serves_another_document_before_continuing_a_busy_one() {
    let probe = Arc::new(Probe::default());
    let supervisor = supervisor(probe.clone(), SupervisorPolicy::default());
    let (alpha, alpha_events, alpha_session) = attach_open(&supervisor, "alpha");
    let (beta, beta_events, beta_session) = attach_open(&supervisor, "beta");

    probe.block_one_render();
    alpha
        .replace_render_viewport(vec![
            render(&alpha_session, 1, DemandPriority::VISIBLE),
            render(&alpha_session, 2, DemandPriority::VISIBLE),
            render(&alpha_session, 3, DemandPriority::VISIBLE),
        ])
        .unwrap();
    probe.wait_until_render_entered();
    beta.replace_render_viewport(vec![render(&beta_session, 9, DemandPriority::VISIBLE)])
        .unwrap();
    probe.release_render();

    // Per-document event back-pressure is released as results are consumed.
    for _ in 0..3 {
        let _ = alpha_events.recv_timeout(TIMEOUT).unwrap();
    }
    expect_render_ready(&beta_events, 9);
    let renders: Vec<_> = probe
        .calls()
        .into_iter()
        .filter(|call| matches!(call, Call::Render(..)))
        .collect();
    assert_eq!(renders[0], Call::Render("alpha", 3));
    assert_eq!(renders[1], Call::Render("beta", 9));
    assert!(matches!(renders[2], Call::Render("alpha", _)));
}

#[test]
fn viewport_replacement_cancels_in_flight_work_and_only_publishes_the_latest_batch() {
    let probe = Arc::new(Probe::default());
    let supervisor = supervisor(probe.clone(), SupervisorPolicy::default());
    let (client, events, session) = attach_open(&supervisor, "alpha");

    probe.block_one_render();
    client
        .replace_render_viewport(vec![render(&session, 1, DemandPriority::VISIBLE)])
        .unwrap();
    probe.wait_until_render_entered();
    client
        .replace_render_viewport(vec![render(&session, 2, DemandPriority::VISIBLE)])
        .unwrap();
    probe.release_render();

    assert!(matches!(
        events.recv_timeout(TIMEOUT).unwrap(),
        SupervisorEvent::Rendered {
            event: RenderEvent::Cancelled { .. },
            ..
        }
    ));
    expect_render_ready(&events, 2);
    assert_eq!(
        probe
            .calls()
            .into_iter()
            .filter(|call| matches!(call, Call::Render(..)))
            .collect::<Vec<_>>(),
        vec![Call::Render("alpha", 1), Call::Render("alpha", 2)]
    );
}

#[test]
fn close_invalidates_the_generation_and_stale_demands_never_enter_the_engine() {
    let probe = Arc::new(Probe::default());
    let supervisor = supervisor(probe.clone(), SupervisorPolicy::default());
    let (client, events, first_session) = attach_open(&supervisor, "alpha");
    let stale = render(&first_session, 1, DemandPriority::VISIBLE);

    client.close().unwrap();
    assert!(matches!(
        events.recv_timeout(TIMEOUT).unwrap(),
        SupervisorEvent::Closed {
            generation: Some(generation),
            ..
        } if generation == first_session.generation()
    ));
    client.open(MockSource("alpha-2")).unwrap();
    let second_session = match events.recv_timeout(TIMEOUT).unwrap() {
        SupervisorEvent::Opened { session, .. } => session,
        other => panic!("expected reopened event, received {other:?}"),
    };
    assert_ne!(first_session.generation(), second_session.generation());

    client.replace_render_viewport(vec![stale]).unwrap();
    assert!(matches!(
        events.recv_timeout(TIMEOUT).unwrap(),
        SupervisorEvent::Rendered {
            event: RenderEvent::Discarded { .. },
            ..
        }
    ));
    assert!(
        !probe
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Render(..)))
    );
}

#[test]
fn an_unread_window_cannot_block_engine_work_for_another_window() {
    let probe = Arc::new(Probe::default());
    let policy = SupervisorPolicy::new(16, 1).unwrap();
    let supervisor = supervisor(probe.clone(), policy);
    let (alpha, alpha_events, alpha_session) = attach_open(&supervisor, "alpha");
    let (beta, beta_events, beta_session) = attach_open(&supervisor, "beta");

    alpha
        .replace_render_viewport(vec![
            render(&alpha_session, 1, DemandPriority::VISIBLE),
            render(&alpha_session, 2, DemandPriority::VISIBLE),
            render(&alpha_session, 3, DemandPriority::VISIBLE),
        ])
        .unwrap();
    // Wait until alpha has filled its one-event channel, but deliberately do
    // not receive it. The owner must rotate to beta instead of blocking.
    let deadline = Instant::now() + TIMEOUT;
    while probe
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Render("alpha", _)))
        .count()
        < 1
    {
        assert!(Instant::now() < deadline, "alpha render did not start");
        thread::yield_now();
    }
    beta.replace_render_viewport(vec![render(&beta_session, 9, DemandPriority::VISIBLE)])
        .unwrap();
    expect_render_ready(&beta_events, 9);

    // Alpha's first result is still intact and can be consumed later.
    assert!(matches!(
        alpha_events.recv_timeout(TIMEOUT).unwrap(),
        SupervisorEvent::Rendered {
            event: RenderEvent::Ready { .. },
            ..
        }
    ));
}

#[test]
fn policy_rejects_zero_sized_bounds() {
    assert_eq!(
        SupervisorPolicy::new(0, 1),
        Err(SupervisorPolicyError::EmptyWorkQueue)
    );
    assert_eq!(
        SupervisorPolicy::new(1, 0),
        Err(SupervisorPolicyError::EmptyEventChannel)
    );
}

#[test]
fn text_demands_cannot_be_sent_through_a_raster_replacement_domain() {
    let probe = Arc::new(Probe::default());
    let supervisor = supervisor(probe, SupervisorPolicy::default());
    let (client, _events, session) = attach_open(&supervisor, "alpha");
    let text = session
        .text_demand(
            0,
            TextDemandPurpose::Copy,
            DemandPriority::INTERACTIVE,
            DemandIntent::Explicit,
        )
        .unwrap();
    assert_eq!(
        client.replace_text(WorkClass::Preview, vec![text]),
        Err(SupervisorSendError::InvalidWorkClass)
    );
}
