/// The granularity of [`WorkerMessage::ReclaimMemory`] in guest physical addresses: the VMM
/// replaces whole blocks only, and the balloon only asks for ranges of whole blocks.
///
/// Free memory in a guest is mostly in pieces smaller than the 2 MiB that Linux reports by
/// default: 60 s after a light load, a 2 GiB guest had 533 MiB free in pieces of 64 KiB to 2 MiB
/// and 81 MiB in smaller ones. With 64 KiB blocks, a guest that reports pieces that small
/// (Linux with `page_reporting.page_reporting_order=4` and 4 KiB pages) gets most of it back to
/// the host. Each block is a VM object in the kernel, about 10 MiB per 2 GiB of guest RAM, and
/// mapping the RAM in blocks takes about 10 ms per GiB at boot.
#[cfg(target_os = "macos")]
pub const RECLAIM_BLOCK_SIZE: u64 = 64 << 10;

#[derive(Debug)]
pub struct MemoryProperties {
    pub gpa: u64,
    pub size: u64,
    pub private: bool,
}

#[derive(Debug)]
pub enum WorkerMessage {
    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    GsiRoute(
        crossbeam_channel::Sender<bool>,
        Vec<kvm_bindings::kvm_irq_routing_entry>,
    ),
    #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
    IrqLine(crossbeam_channel::Sender<bool>, u32, bool),
    #[cfg(target_os = "macos")]
    GpuAddMapping(crossbeam_channel::Sender<bool>, u64, u64, u64),
    #[cfg(target_os = "macos")]
    GpuRemoveMapping(crossbeam_channel::Sender<bool>, u64, u64),
    /// Gives the host pages backing a range of guest RAM back to the system; the range reads
    /// as zeroes afterwards. Replies whether they were given back. The range is made of whole
    /// blocks of [`RECLAIM_BLOCK_SIZE`], or the VMM leaves it as it is.
    #[cfg(target_os = "macos")]
    ReclaimMemory(crossbeam_channel::Sender<bool>, u64, u64),
    #[cfg(not(target_os = "windows"))]
    ConvertMemory(crossbeam_channel::Sender<bool>, MemoryProperties),
}
