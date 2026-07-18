//! PDF worker boundary used by the standalone reader shell.

mod client;
mod protocol;
mod worker;

pub use client::PdfWorker;
pub use protocol::{PreviewSpec, RenderAppearance, RenderColor, TileRequest, WorkerEvent};
pub(crate) use worker::{PdfEngineSupervisor, start_pdf_engine_supervisor};
