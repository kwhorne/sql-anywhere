use std::mem::size_of;

use sqlanywhere_wal::replication::injector::Injector;
use sqlanywhere_wal::segment::sealed::SealedSegment;
use sqlanywhere_wal::segment::Frame as WalFrame;
use sqlanywhere_wal::{io::StdIO, storage::Storage};
use zerocopy::{AsBytes, FromZeroes};

use crate::frame::FrameNo;
use crate::rpc::replication::Frame as RpcFrame;

use super::error::{Error, Result};

pub struct SqlanywhereInjector<S> {
    injector: Injector<StdIO, S>,
}

impl<S> SqlanywhereInjector<S> {
    pub fn new(injector: Injector<StdIO, S>) -> Self {
        Self { injector }
    }
}

impl<S: Storage<Segment = SealedSegment<std::fs::File>>> super::Injector for SqlanywhereInjector<S> {
    async fn inject_frame(&mut self, frame: RpcFrame) -> Result<Option<FrameNo>> {
        // this is a bit annoying be we want to read the frame, and it has to be aligned, so we
        // must copy it...
        // FIXME: optimize this.
        let mut wal_frame = WalFrame::new_box_zeroed();
        if frame.data.len() != size_of::<WalFrame>() {
            todo!("invalid frame");
        }
        wal_frame.as_bytes_mut().copy_from_slice(&frame.data[..]);

        Ok(self
            .injector
            .insert_frame(wal_frame)
            .await
            .map_err(|e| Error::FatalInjectError(e.into()))?)
    }

    async fn rollback(&mut self) {
        self.injector.rollback();
    }

    async fn flush(&mut self) -> Result<Option<FrameNo>> {
        self.injector
            .flush(None)
            .await
            .map_err(|e| Error::FatalInjectError(e.into()))?;
        Ok(None)
    }

    fn durable_frame_no(&mut self, frame_no: u64) {
        self.injector.set_durable(frame_no);
    }
}
