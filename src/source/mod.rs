//! Log sources. Each produces a stream of `Entry` values; filtering and output are shared.

pub mod device;
pub mod dnssd;
pub mod mac;
pub mod remote;
pub mod simulator;

use std::pin::Pin;

use anyhow::Result;
use futures::Stream;

use crate::entry::Entry;

pub type Entries = Pin<Box<dyn Stream<Item = Result<Entry>> + Send>>;
