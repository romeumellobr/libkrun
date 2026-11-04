//! FUSE requests over a vendor hypercall, answered on the vCPU thread.
//!
//! A small synchronous request (lookup, getattr, create, unlink...) spends most of
//! its time on the virtqueue round trip: the queue notification, waking the worker
//! thread, and the interrupt back. A guest driver that knows about this path can
//! instead describe the request in guest memory and issue an HVC; the server
//! handles it on the calling vCPU and the reply is in place when the HVC returns.
//! Anything else keeps using the virtqueue.
//!
//! Calls (x0 = function, x1/x2 = arguments, result in x0):
//! - [`HVC_FS_QUERY`]: x1 = guest address of the device tag, NUL-terminated, at
//!   most 36 bytes. Returns a device id, or `u64::MAX` if the tag is unknown.
//! - [`HVC_FS_REQUEST`]: x1 = device id, x2 = guest address of an [`HvcTable`].
//!   Returns the number of bytes written to the reply buffers, or `u64::MAX` if
//!   the request was refused before the server saw it (the guest may then send
//!   it through the virtqueue).

use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;
#[cfg(target_os = "macos")]
use utils::worker_message::WorkerMessage;
use vm_memory::{ByteValued, Bytes, GuestAddress, GuestMemoryMmap};

use super::descriptor_utils::{Reader, Writer};
use super::worker::FsServer;
use crate::virtio::VirtioShmRegion;

pub const HVC_FS_QUERY: u64 = 0xC600_0100;
pub const HVC_FS_REQUEST: u64 = 0xC600_0101;

/// Most segments a request may describe: the FUSE header, a few arguments and
/// their reply, as the guest builds them for a small request.
const MAX_SEGS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HvcSeg {
    pub addr: u64,
    pub len: u32,
    pub _pad: u32,
}

/// Request descriptor in guest memory: `n_out` device-readable segments (the
/// request) followed by `n_in` device-writable ones (the reply).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HvcTable {
    pub n_out: u32,
    pub n_in: u32,
    pub segs: [HvcSeg; MAX_SEGS],
}

// SAFETY: plain old data with no implicit padding.
unsafe impl ByteValued for HvcSeg {}
// SAFETY: plain old data with no implicit padding.
unsafe impl ByteValued for HvcTable {}

pub(crate) struct HvcDevice {
    pub tag: String,
    pub server: Arc<FsServer>,
    pub mem: GuestMemoryMmap,
    pub allow_idmap: bool,
    pub shm_region: Option<VirtioShmRegion>,
    pub exit_code: Arc<AtomicI32>,
    #[cfg(target_os = "macos")]
    pub map_sender: Option<Sender<WorkerMessage>>,
}

static DEVICES: Mutex<Vec<Arc<HvcDevice>>> = Mutex::new(Vec::new());

/// Makes a device reachable through the hypercall. A device that is activated
/// again (guest reboot) replaces its earlier entry and keeps its id.
pub(crate) fn register(dev: HvcDevice) {
    let mut devs = DEVICES.lock().unwrap();
    let dev = Arc::new(dev);
    match devs.iter().position(|d| d.tag == dev.tag) {
        Some(i) => devs[i] = dev,
        None => devs.push(dev),
    }
    #[cfg(target_os = "macos")]
    let _ = hvf::VENDOR_HVC.set(handle);
}

pub fn handle(func: u64, x1: u64, x2: u64) -> u64 {
    match func {
        HVC_FS_QUERY => query(x1).unwrap_or(u64::MAX),
        HVC_FS_REQUEST => request(x1, x2).unwrap_or(u64::MAX),
        _ => u64::MAX,
    }
}

fn query(tag_addr: u64) -> Option<u64> {
    let devs = DEVICES.lock().unwrap();
    let mem = &devs.first()?.mem;
    let mut raw = [0u8; 36];
    mem.read_slice(&mut raw, GuestAddress(tag_addr)).ok()?;
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let tag = std::str::from_utf8(&raw[..end]).ok()?;
    devs.iter().position(|d| d.tag == tag).map(|i| i as u64)
}

fn request(id: u64, table_addr: u64) -> Option<u64> {
    let dev = DEVICES.lock().unwrap().get(id as usize)?.clone();
    let table: HvcTable = dev.mem.read_obj(GuestAddress(table_addr)).ok()?;
    let (n_out, n_in) = (table.n_out as usize, table.n_in as usize);
    if n_out == 0 || n_out + n_in > MAX_SEGS {
        return None;
    }
    let segs = |range: std::ops::Range<usize>| {
        table.segs[range]
            .iter()
            .map(|s| (GuestAddress(s.addr), s.len))
            .collect::<Vec<_>>()
    };
    let reader = Reader::from_guest_buffers(&dev.mem, segs(0..n_out)).ok()?;
    let writer = Writer::from_guest_buffers(&dev.mem, segs(n_out..n_out + n_in)).ok()?;
    dev.server
        .handle_message(
            reader,
            writer,
            dev.allow_idmap,
            &dev.shm_region,
            &dev.exit_code,
            #[cfg(target_os = "macos")]
            &dev.map_sender,
        )
        // A failure here may come after the server acted on the request, so the
        // guest must not resend it: report an empty reply, as the virtqueue does.
        .map_or(Some(0), |len| Some(len as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_layout_matches_the_guest() {
        // the guest driver lays the table out the same way: 2 u32, then 16-byte segments
        assert_eq!(std::mem::size_of::<HvcSeg>(), 16);
        assert_eq!(std::mem::size_of::<HvcTable>(), 8 + 16 * MAX_SEGS);
    }

    #[test]
    fn unknown_calls_and_devices_are_refused() {
        assert_eq!(handle(0xC600_01FF, 0, 0), u64::MAX);
        assert_eq!(handle(HVC_FS_REQUEST, 9999, 0), u64::MAX);
    }
}
