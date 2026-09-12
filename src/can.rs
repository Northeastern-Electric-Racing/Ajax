use anyhow::{Context, Result};
use socketcan::{
    CanFrame as SocketCanFrame,
    CanSocket as RawCanSocket,
    EmbeddedFrame,
    Frame,
    Socket,
};
use std::{io, time::Duration};

#[derive(Debug, Clone)]
pub struct CanFrame {
    pub id: u32,
    pub data: Vec<u8>,
}

pub struct SocketCan {
    socket: RawCanSocket,
}

impl SocketCan {
    pub fn open(interface: &str) -> Result<Self> {
        let socket = RawCanSocket::open(interface)
            .with_context(|| format!("failed to open CAN interface '{interface}'"))?;

        Ok(Self { socket })
    }

    pub fn send(&self, id: u32, data: &[u8]) -> Result<()> {
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

