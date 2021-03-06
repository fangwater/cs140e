use std::collections::VecDeque;

use mutex::Mutex;
use process::{Process, State, Id};
use traps::TrapFrame;
use pi::interrupt::{Interrupt, Controller};
use pi::timer::tick_in;
use aarch64;
use run_blinky;
use run_shell;

/// The `tick` time.
pub const TICK: u32 = 10 * 1000;

/// Process scheduler for the entire machine.
#[derive(Debug)]
pub struct GlobalScheduler(Mutex<Option<Scheduler>>);

impl GlobalScheduler {
    /// Returns an uninitialized wrapper around a local scheduler.
    pub const fn uninitialized() -> GlobalScheduler {
        GlobalScheduler(Mutex::new(None))
    }

    /// Adds a process to the scheduler's queue and returns that process's ID.
    /// For more details, see the documentation on `Scheduler::add()`.
    pub fn add(&self, process: Process) -> Option<Id> {
        self.0.lock().as_mut().expect("scheduler uninitialized").add(process)
    }

    /// Performs a context switch using `tf` by setting the state of the current
    /// process to `new_state`, saving `tf` into the current process, and
    /// restoring the next process's trap frame into `tf`. For more details, see
    /// the documentation on `Scheduler::switch()`.
    #[must_use]
    pub fn switch(&self, new_state: State, tf: &mut TrapFrame) -> Option<Id> {
        self.0.lock().as_mut().expect("scheduler uninitialized").switch(new_state, tf)
    }

    /// Initializes the scheduler and starts executing processes in user space
    /// using timer interrupt based preemptive scheduling. This method should
    /// not return under normal conditions.
    pub fn start(&self) {
        *self.0.lock() = Some(Scheduler::new());

        let mut process = Process::new().expect("First process failed");
        process.trap_frame.elr = run_shell as u64;
        process.trap_frame.sp = process.stack.top().as_u64();
        // Don't mask DAIF, set execution level to 0.
        process.trap_frame.spsr = 0x0;
        let tf = process.trap_frame.clone();

        self.add(process);

        let mut process_1 = Process::new().unwrap();
        process_1.trap_frame.elr = run_blinky as u64;
        process_1.trap_frame.sp = process_1.stack.top().as_u64();
        self.add(process_1);

        Controller::new().enable(Interrupt::Timer1);
        tick_in(TICK);

        // Switch to process.
        unsafe {
            asm!(
                "// Set the SP to the start of the trap frame.
                 mov SP, $0

                 // Restore the trap frame registers.
                 bl context_restore

                 // Reset SP to _start.
                 adr x0, _start
                 mov SP, x0
                 mov x0, xzr
                 mov lr, xzr

                 // Start the process.
                 eret"
                 :: "r"(tf)
                 :: "volatile");
        }
    }
}

#[derive(Debug)]
struct Scheduler {
    processes: VecDeque<Process>,
    current: Option<Id>,
    last_id: Option<Id>,
}

impl Scheduler {
    /// Returns a new `Scheduler` with an empty queue.
    fn new() -> Scheduler {
        Scheduler {
            processes: VecDeque::new(),
            current: None,
            last_id: None
        }
    }

    /// Adds a process to the scheduler's queue and returns that process's ID if
    /// a new process can be scheduled. The process ID is newly allocated for
    /// the process and saved in its `trap_frame`. If no further processes can
    /// be scheduled, returns `None`.
    ///
    /// If this is the first process added, it is marked as the current process.
    /// It is the caller's responsibility to ensure that the first time `switch`
    /// is called, that process is executing on the CPU.
    fn add(&mut self, mut process: Process) -> Option<Id> {
        let id = match self.last_id {
            Some(last_id) => last_id.checked_add(1)?,
            None => 0
        };

        process.trap_frame.tpidr = id;
        self.processes.push_back(process);

        if let None = self.current {
            self.current = Some(id);
        }

        self.last_id = Some(id);
        self.last_id
    }

    /// Sets the current process's state to `new_state`, finds the next process
    /// to switch to, and performs the context switch on `tf` by saving `tf`
    /// into the current process and restoring the next process's trap frame
    /// into `tf`. If there is no current process, returns `None`. Otherwise,
    /// returns `Some` of the process ID that was context switched into `tf`.
    ///
    /// This method blocks until there is a process to switch to, conserving
    /// energy as much as possible in the interim.
    fn switch(&mut self, new_state: State, tf: &mut TrapFrame) -> Option<Id> {
        let mut current = self.processes.pop_front()?;
        let current_id = current.get_id();
        current.trap_frame = Box::new(*tf);
        current.state = new_state;
        self.processes.push_back(current);

        loop {
            let mut process = self.processes.pop_front()?;
            if process.is_ready() {
                self.current = Some(process.get_id() as Id);
                *tf = *process.trap_frame;
                process.state = State::Running;

                // Push process back into queue.
                self.processes.push_front(process);
                break;
            } else if process.get_id() == current_id {
                // We cycled the list, wait for an interrupt.
                aarch64::wfi();
            }

            self.processes.push_back(process);
        }

        self.current
    }
}

