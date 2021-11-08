
use super::*;
use core::mem::zeroed;

pub fn process_name(pid: pid_t) -> Option<String> {
    read_lines(format!("/proc/{}/comm", pid)).ok()?.next()
}

pub fn process_cmdline(pid: pid_t) -> Vec<String> {
    let data = std::fs::read(format!("/proc/{}/cmdline", pid)).unwrap_or(vec![]);
    let mut result = data.split(|b| *b == 0u8).map(|b| unsafe {
        String::from_utf8_unchecked(b.to_vec())
    }).collect::<Vec<_>>();
    while result.last().map(String::is_empty).unwrap_or(false) {
        result.pop();
    }
    result
}

pub fn process_path(pid: pid_t) -> Option<String> {
    read_link(format!("/proc/{}/exe", pid)).ok()?.to_str()
                             .map(|path| path.to_string())
}

pub fn process_tasks(pid: pid_t) -> PidIter {
    PidIter(read_dir(format!("/proc/{}/task", pid)).ok())
}

pub fn process_fd(pid: pid_t) -> Option<impl Iterator<Item = (usize, PathBuf)>> {
    Some(PidIter(Some(read_dir(format!("/proc/{}/fd", pid)).ok()?)).filter_map(move |id|
        Some((id as usize, read_link(format!("/proc/{}/fd/{}", pid, id)).ok()?))
    ))
}

pub fn process_environ(pid: pid_t) -> HashMap<String, String> {
    let data = std::fs::read(format!("/proc/{}/environ", pid)).unwrap_or(vec![]);
    let mut result = HashMap::new();
    data.split(|b| *b == 0u8).map(|b| unsafe {
        let item = std::str::from_utf8_unchecked(b);
        let mut i = item.split("=");
        if let Some(name) = i.next() {
            result.insert(name.to_string(), i.next().unwrap().into());
        }
    });
    result
}

pub struct Process {
    pub pid: pid_t,
    mem: RwLock<Option<Box<File>>>,
}

impl Process {
    pub fn from_pid(pid: pid_t) -> Option<Self> {
        if Path::new(&format!("/proc/{}", pid)).exists() {
            Some(Self { pid, mem: RwLock::new(None), })
        } else { None }
    }

    pub fn from_comm(name: &str) -> Option<Self> {
        enum_pid().find(|&pid| process_name(pid).as_ref().map(String::as_str) == Some(name)).and_then(Process::from_pid)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        enum_pid().find(|&pid| process_cmdline(pid).get(0).map(String::as_str) == Some(name)).and_then(Process::from_pid)
    }

    pub fn current() -> Self {
        unsafe { Self::from_pid(getpid()).unwrap() }
    }

    pub fn pid(&self) -> pid_t { self.pid }

    #[inline]
    pub fn name(&self) -> Option<String> { process_name(self.pid) }

    #[inline]
    pub fn cmdline(&self) -> Vec<String> { process_cmdline(self.pid) }

    #[inline]
    pub fn image_path(&self) -> Option<String> { process_path(self.pid) }

    #[inline]
    pub fn environ(&self) -> HashMap<String, String> { process_environ(self.pid) }

    pub fn read_mem(mem: &mut File, address: usize, buf: &mut [u8]) -> usize {
        mem.seek(SeekFrom::Start(address as u64)).and_then(|_| mem.read(buf)).unwrap_or(0)
    }

    pub fn read<'a>(&self, address: usize, buf: &'a mut [u8]) -> Option<&'a mut [u8]> {
        if self.mem.read().unwrap().is_none() {
            *self.mem.write().unwrap() = Some(Box::new(File::open(format!("/proc/{}/mem", self.pid)).ok()?));
        }
        self.mem.write().unwrap().as_mut().and_then(move |f| {
            let result = Self::read_mem(f, address, buf);
            if result > 0 { Some(&mut buf[..result]) } else { None }
        })
    }

    pub fn write(&self, address: usize, buf: &[u8]) -> Option<usize> {
        let mut mem = File::create(format!("/proc/{}/mem", self.pid)).ok()?;
        mem.seek(SeekFrom::Start(address as u64)).ok()?;
        mem.write(buf).ok()
    }

    fn lines(&self, subpath: &str) -> io::Result<LineReader<File>> {
        read_lines(format!("/proc/{}/{}", self.pid, subpath))
    }

    pub fn enum_memory(&self) -> Result<MemoryIter, String> {
        Ok(MemoryIter(self.lines("maps").map_err(|e| format!("{}", e))?))
    }

    #[inline]
    pub fn enum_thread(&self) -> impl Iterator<Item = pid_t> {
        process_tasks(self.pid)
    }

    pub fn enum_module(&self) -> Result<ModuleIter, String> {
        Ok(ModuleIter {
            f: read_lines(format!("/proc/{}/maps", self.pid)).map_err(|e| format!("{}", e))?,
            p: self, base: 0, size: 0, usage: "".into(), cached: false,
        })
    }

    pub fn find_module_by_name(&self, name: &str) -> Option<Module> {
        self.enum_module().ok()?.find(|m| m.name.as_ref() == name)
    }

    pub fn wait(&self, opt: c_int) -> (pid_t, WaitStatus) {
        let mut status: c_int = 0;
        unsafe {
            // http://man7.org/linux/man-pages/man2/waitpid.2.html
            let tid = libc::waitpid(-1, &mut status, opt);

            if WIFEXITED(status) {
                return (tid, WaitStatus::Exit(WEXITSTATUS(status)));
            }
            if WIFSIGNALED(status) {
                return (tid, WaitStatus::Signal(WTERMSIG(status)));
            }
            if WIFSTOPPED(status) {
                let extra = (status >> 16) & 0xffff;
                return (tid, match extra {
                    PTRACE_EVENT_CLONE => {
                        let new_tid: pid_t = 0;
                        if ptrace(PTRACE_GETEVENTMSG, tid, 0, &new_tid) < 0 {
                            panic!("PTRACE_GETEVENTMSG Failed");
                        }
                        WaitStatus::Clone(new_tid)
                    }
                    PTRACE_EVENT_EXEC => {
                        WaitStatus::Exec
                    }
                    _ => WaitStatus::Stop(WSTOPSIG(status)),
                });
            }
            if WIFCONTINUED(status) {
                return (tid, WaitStatus::Continue);
            }
            panic!("invalid waitpid");
        }
    }

    pub fn get_regs(&self, tid: pid_t) -> Option<user_regs_struct> {
        unsafe {
            let mut regs: user_regs_struct = zeroed();
            if ptrace_getregs(tid, &mut regs) { Some(regs) } else { None }
        }
    }

    pub fn siginfo(&self, tid: pid_t) -> Option<siginfo_t> {
        unsafe {
            let info: libc::siginfo_t = zeroed();
            if ptrace(PTRACE_GETSIGINFO, tid, 0, &info) >= 0 {
                Some(info)
            } else { None }
        }
    }
}

impl ReadMemory for Process {
    fn read_memory<'a>(&self, addr: usize, data: &'a mut [u8]) -> Option<&'a mut [u8]> {
        self.read(addr, data)
    }
}

impl WriteMemory for Process {
    fn write_memory(&self, address: usize, data: &[u8]) -> Option<usize> {
        self.write(address, data)
    }
}