use anyhow::{Context, Result};
use socketcan::{
    CanFrame as SocketCanFrame,
    CanSocket as RawCanSocket,
    EmbeddedFrame,
    Frame,
    Socket,
};
use std::{io, time::{Duration, Instant}};

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct CanFrame {
    pub id: u32,
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

        Ok(Self { socket, cancelled: None })
    }

    pub fn with_cancellation(mut self, cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Result<Self> {
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

    pub fn send(&self, id: u32, data: &[u8]) -> Result<()> {
        self.check_cancelled()?;
        anyhow::ensure!(
            data.len() <= 8,
            "Classical CAN payload exceeds 8 bytes"
        );

        let frame = SocketCanFrame::from_raw_id(id, data)
            .ok_or_else(|| anyhow::anyhow!("CAN identifier 0x{id:X} is out of range"))?;

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
        match self.socket.read_frame_timeout(timeout) {
            Ok(frame) => Ok(Some(CanFrame {
                id: frame.raw_id(),
                data: frame.data().to_vec(),
            })),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(error) => Err(error).context("CAN receive failed"),
        }
    }

    pub fn clear(&self) -> Result<()> {
        while self.receive(Duration::ZERO)?.is_some() {}
        Ok(())
    }
}

