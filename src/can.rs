use anyhow::{Context, Result};
use socketcan::{
    CanFilter, CanFrame as SocketCanFrame, CanSocket as RawCanSocket, EmbeddedFrame, Id, Socket,
    SocketOptions,
    id::{CAN_EFF_FLAG, CAN_EFF_MASK, CAN_RTR_FLAG, CAN_SFF_MASK, id_to_canid_t},
};
use std::{
    io,
    time::{Duration, Instant},
};

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct CanFrame {
    pub id: Id,
    pub data: Vec<u8>,
}

pub struct SocketCan {
    socket: RawCanSocket,
    cancelled: Option<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl SocketCan {
    pub fn open(interface: &str) -> Result<Self> {
        let socket = RawCanSocket::open(interface)
            .with_context(|| format!("failed to open CAN interface '{interface}'"))?;

        Ok(Self {
            socket,
            cancelled: None,
        })
    }

    /// Only receive data frames with the ECU's response ID from ecus.json.
    pub fn filter(&self, id: Id) -> Result<()> {
        let mask = match id {
            Id::Standard(_) => CAN_SFF_MASK,
            Id::Extended(_) => CAN_EFF_MASK,
        } | CAN_EFF_FLAG
            | CAN_RTR_FLAG;
        self.socket
            .set_filters(&[CanFilter::new(id_to_canid_t(id), mask)])
            .context("failed to set CAN receive filter")
    }

    pub fn with_cancellation(
        mut self,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        // Bound a blocked write so cancellation can release the CAN interface.
        self.socket.set_write_timeout(CANCEL_POLL_INTERVAL)?;
        self.cancelled = Some(Box::new(cancelled));
        Ok(self)
    }

    pub fn check_cancelled(&self) -> Result<()> {
        anyhow::ensure!(
            !self.cancelled.as_ref().is_some_and(|cancelled| cancelled()),
            "Flash cancelled: client disconnected"
        );
        Ok(())
    }

    pub fn send(&self, id: Id, data: &[u8]) -> Result<()> {
        self.check_cancelled()?;
        anyhow::ensure!(data.len() <= 8, "Classical CAN payload exceeds 8 bytes");

        let frame = SocketCanFrame::new(id, data)
            .ok_or_else(|| anyhow::anyhow!("could not construct CAN data frame"))?;

        self.socket
            .write_frame(&frame)
            .context("CAN transmit failed")
    }

    pub fn receive(&self, timeout: Duration) -> Result<Option<CanFrame>> {
        if self.cancelled.is_none() {
            return self.receive_once(timeout);
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.check_cancelled()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = self.receive_once(remaining.min(CANCEL_POLL_INTERVAL))?;
            self.check_cancelled()?;
            if frame.is_some() || Instant::now() >= deadline {
                return Ok(frame);
            }
        }
    }

    fn receive_once(&self, timeout: Duration) -> Result<Option<CanFrame>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.socket.read_frame_timeout(remaining) {
                Ok(SocketCanFrame::Data(frame)) => {
                    return Ok(Some(CanFrame {
                        id: frame.id(),
                        data: frame.data().to_vec(),
                    }));
                }
                // Skip remote and error frames without ending the wait early.
                Ok(_) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error).context("CAN receive failed"),
            }
        }
    }

    pub fn clear(&self) -> Result<()> {
        while self.receive(Duration::ZERO)?.is_some() {}
        Ok(())
    }
}
