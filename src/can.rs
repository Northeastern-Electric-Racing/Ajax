use anyhow::{Context, Result};
use std::{io, mem, os::fd::RawFd, time::Duration};

const CAN_RAW: i32 = 1;
const CAN_EFF_FLAG: u32 = 0x8000_0000;
const CAN_SFF_MASK: u32 = 0x0000_07FF;
const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;

#[repr(C)]
struct SockAddrCan {
    can_family: libc::sa_family_t,
    can_ifindex: i32,
    addr: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SocketCanFrame {
    can_id: u32,
    can_dlc: u8,
    pad: u8,
    res0: u8,
    len8_dlc: u8,
    data: [u8; 8],
}

#[derive(Debug, Clone)]
pub struct CanFrame {
    pub id: u32,
    pub data: Vec<u8>,
}



pub struct SocketCan {
    fd: RawFd,
}

impl SocketCan {
    pub fn open(interface: &str) -> Result<Self> {
        let fd = unsafe { libc::socket(libc::PF_CAN, libc::SOCK_RAW, CAN_RAW) };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("socket(PF_CAN) failed");
        }

        let cname = std::ffi::CString::new(interface)?;
        let ifindex = unsafe { libc::if_nametoindex(cname.as_ptr()) };
        if ifindex == 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error).with_context(|| format!("CAN interface '{interface}' not found"));
        }

        let addr = SockAddrCan {
            can_family: libc::AF_CAN as libc::sa_family_t,
            can_ifindex: ifindex as i32,
            addr: [0; 8],
        };

        let result = unsafe {
            libc::bind(
                fd,
                &addr as *const SockAddrCan as *const libc::sockaddr,
                mem::size_of::<SockAddrCan>() as libc::socklen_t,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(error).with_context(|| format!("failed to bind {interface}"));
        }

        Ok(Self { fd })
    }

    pub fn send(&self, id: u32, data: &[u8]) -> Result<()> {
        anyhow::ensure!(data.len() <= 8, "Classical CAN payload exceeds 8 bytes");
        let can_id = if id <= CAN_SFF_MASK {
            id
        } else if id <= CAN_EFF_MASK {
            id | CAN_EFF_FLAG
        } else {
            anyhow::bail!("CAN identifier 0x{id:X} is out of range");
        };

        let mut frame = SocketCanFrame {
            can_id,
            can_dlc: data.len() as u8,
            pad: 0,
            res0: 0,
            len8_dlc: 0,
            data: [0; 8],
        };
        frame.data[..data.len()].copy_from_slice(data);

        let written = unsafe {
            libc::write(
                self.fd,
                &frame as *const SocketCanFrame as *const libc::c_void,
                mem::size_of::<SocketCanFrame>(),
            )
        };
        if written != mem::size_of::<SocketCanFrame>() as isize {
            return Err(io::Error::last_os_error()).context("CAN transmit failed");
        }
        Ok(())
    }

    pub fn receive(&self, timeout: Duration) -> Result<Option<CanFrame>> {
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut pollfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };

        loop {
            let status = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if status == 0 {
                return Ok(None);
            }
            if status < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error).context("CAN poll failed");
            }
            break;
        }

        let mut frame: SocketCanFrame = unsafe { mem::zeroed() };
        let read = unsafe {
            libc::read(
                self.fd,
                &mut frame as *mut SocketCanFrame as *mut libc::c_void,
                mem::size_of::<SocketCanFrame>(),
            )
        };
        if read != mem::size_of::<SocketCanFrame>() as isize {
            return Err(io::Error::last_os_error()).context("CAN receive failed");
        }

        let id = if frame.can_id & CAN_EFF_FLAG != 0 {
            frame.can_id & CAN_EFF_MASK
        } else {
            frame.can_id & CAN_SFF_MASK
        };
        let len = usize::from(frame.can_dlc.min(8));
        Ok(Some(CanFrame {
            id,
            data: frame.data[..len].to_vec(),
        }))
    }

    pub fn clear(&self) -> Result<()> {
        while self.receive(Duration::ZERO)?.is_some() {}
        Ok(())
    }
}

impl Drop for SocketCan {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
