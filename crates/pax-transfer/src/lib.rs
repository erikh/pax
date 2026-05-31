//! # pax-transfer
//!
//! High-level file upload for the [`pax`](https://github.com/erikh/pax) toolkit.
//! It wraps the raw
//! [`push_file`](pax_transport::BluetoothBackend::push_file) primitive with the
//! connection lifecycle, retries, multi-file batches, and a clean
//! [`progress`] callback distilled from the event stream.
//!
//! ```
//! use pax_core::DeviceId;
//! use pax_transport::mock::{MockBackend, MockDevice};
//! use pax_transfer::{upload_bytes, UploadOptions};
//!
//! # async fn run() -> Result<(), pax_transport::TransportError> {
//! let id: DeviceId = "AA:BB:CC:DD:EE:FF".parse().unwrap();
//! let backend = MockBackend::builder().device(MockDevice::new(id, "Phone")).build();
//! let receipt = upload_bytes(&backend, id, "hello.txt", b"hi there", UploadOptions::default()).await?;
//! println!("sent {} bytes at {:.0} B/s", receipt.bytes, receipt.throughput_bytes_per_sec());
//! # Ok(()) }
//! ```
//!
//! For a live progress bar, attach a [`progress::ProgressObserver`] to the backend
//! (via [`pax_core::observe::FanOut`] if you also want diagnostics).
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod progress;
pub mod upload;

pub use progress::{ProgressObserver, ProgressUpdate};
pub use upload::{upload_bytes, upload_file, upload_files, UploadOptions};
