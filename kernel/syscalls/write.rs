use super::MAX_READ_WRITE_LEN;
use crate::{arch::UserVAddr, fs::opened_file::Fd, result::Result};
use crate::{process::current_process, syscalls::SyscallDispatcher};
use core::cmp::min;

impl SyscallDispatcher {
    pub fn sys_write(&mut self, fd: Fd, uaddr: UserVAddr, len: usize) -> Result<isize> {
        let len = min(len, MAX_READ_WRITE_LEN);

        let mut buf = vec![0; len]; // TODO: deny too long len
        uaddr.read_bytes(&mut buf)?;
        let current = current_process().opened_files.lock();
        let open_file = current.get(fd)?;
        let file = open_file.as_file()?;

        // MAX_READ_WRITE_LEN limit guarantees total_len is in the range of isize.
        let written_len = file.write(open_file.pos(), buf.as_slice())?;

        // MAX_READ_WRITE_LEN limit guarantees total_len is in the range of isize.
        Ok(written_len as isize)
    }
}