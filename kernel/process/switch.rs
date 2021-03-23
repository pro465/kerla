use super::*;
use crate::process::PId;
use crate::{
    arch::{self, disable_interrupt, enable_interrupt, is_interrupt_enabled, SpinLock, VAddr},
    elf::Elf,
    fs::initramfs::INITRAM_FS,
    fs::mount::RootFs,
    fs::opened_file,
    fs::path::Path,
    fs::{
        devfs::DEV_FS,
        inode::{FileLike, INode},
        opened_file::*,
        stat::Stat,
    },
    mm::{
        page_allocator::alloc_pages,
        vm::{Vm, VmAreaType},
    },
    result::{Errno, Error, ErrorExt, Result},
};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use arch::{UserVAddr, KERNEL_STACK_SIZE, PAGE_SIZE, USER_STACK_TOP};
use arrayvec::ArrayVec;
use core::cmp::max;
use core::mem::{self, size_of, size_of_val};
use core::sync::atomic::{AtomicI32, Ordering};
use goblin::elf64::program_header::PT_LOAD;
use opened_file::OpenedFileTable;
use penguin_utils::once::Once;
use penguin_utils::{alignment::align_up, lazy::Lazy};

cpu_local! {
    static ref HELD_LOCKS: ArrayVec<[Arc<Process>; 2]> = ArrayVec::new();
}

/// Yields execution to another thread. When the currently running thread is resumed
// in future, it will be
pub fn switch(new_state: ProcessState) {
    // Save the current interrupt enable flag to restore it in the next execution
    // of the currently running thread.
    let interrupt_enabled = is_interrupt_enabled();

    let prev_thread = CURRENT.get();
    let next_thread = {
        let mut scheduler = SCHEDULER.lock();

        // Push back the currently running thread to the runqueue if it's still
        // ready for running, in other words, it's not blocked.
        if prev_thread.pid != PId::new(0) && new_state == ProcessState::Runnable {
            scheduler.enqueue((*prev_thread).clone());
        }

        // Pick a thread to run next.
        match scheduler.pick_next() {
            Some(next) => next,
            None if prev_thread.is_idle() => return,
            None => IDLE_THREAD.get().get().clone(),
        }
    };

    assert!(HELD_LOCKS.get().is_empty());
    assert!(!Arc::ptr_eq(prev_thread, &next_thread));

    // Save locks that will be released later.
    debug_assert!(HELD_LOCKS.get().is_empty());
    HELD_LOCKS.as_mut().push((*prev_thread).clone());
    HELD_LOCKS.as_mut().push(next_thread.clone());

    // Since these locks won't be dropped until the current (prev) thread is
    // resumed next time, we'll unlock these locks in `after_switch` in the next
    // thread's context.
    let mut prev_inner = prev_thread.inner.lock();
    let mut next_inner = next_thread.inner.lock();

    if let Some(vm) = next_thread.vm.as_ref() {
        let lock = vm.lock();
        lock.page_table().switch();
    }

    // Switch into the next thread.
    CURRENT.as_mut().set(next_thread.clone());
    arch::switch_thread(&mut (*prev_inner).arch, &mut (*next_inner).arch);

    // Don't call destructors as they're unlocked in `after_switch`.
    mem::forget(prev_inner);
    mem::forget(next_inner);

    // Now we're in the next thread. Release held locks and continue executing.
    after_switch();

    // Retstore the interrupt enable flag manually because lock guards
    // (`prev` and `next`) that holds the flag state are `mem::forget`-ed.
    if interrupt_enabled {
        unsafe {
            enable_interrupt();
        }
    }
}

#[no_mangle]
pub extern "C" fn after_switch() {
    for thread in HELD_LOCKS.as_mut().drain(..) {
        unsafe {
            thread.inner.force_unlock();
        }
    }
}