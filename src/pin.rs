use std::io;
use std::mem;

use libc::{cpu_set_t, sched_setaffinity, CPU_SET, CPU_ZERO};

pub fn pin_current_thread_to_cpu(cpu_index: usize) -> io::Result<()> {
    unsafe {
        let mut set: cpu_set_t = mem::zeroed();
        CPU_ZERO(&mut set);
        CPU_SET(cpu_index, &mut set);

        let res = sched_setaffinity(0, mem::size_of::<cpu_set_t>(), &set);
        if res != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

