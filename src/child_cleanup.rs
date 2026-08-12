//! Windows 子进程联动退出：把当前进程加入 KILL_ON_JOB_CLOSE 的 Job Object，
//! 之后启动的子进程（chrome、typst watch 等）自动加入该作业；
//! 服务以任何方式退出（正常停止、Ctrl+C、关闭控制台、被终止）时，
//! OS 关闭作业句柄并连带终止其中的所有子进程，避免浏览器等进程残留。
#![cfg(windows)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use tracing::warn;

type Handle = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct JobObjectExtendedLimitInformation {
    basic: JobObjectBasicLimitInformation,
    io_info: [u64; 6],
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, info_class: i32, info: *const c_void, len: u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn GetCurrentProcess() -> Handle;
    #[cfg(test)]
    fn IsProcessInJob(process: Handle, job: Handle, result: *mut i32) -> i32;
}

/// 把当前进程放入 KILL_ON_JOB_CLOSE 作业（幂等）。
/// 作业句柄有意保持打开直到进程结束，由 OS 在退出时回收并级联终止子进程。
/// 失败（如进程已被其它 Job 管理且不允许嵌套）只记录告警，不影响服务启动。
pub fn setup_kill_on_close_job() -> bool {
    static INIT: Once = Once::new();
    static OK: AtomicBool = AtomicBool::new(false);
    INIT.call_once(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            warn!("创建 Job Object 失败，子进程联动退出不可用");
            return;
        }
        let mut info = JobObjectExtendedLimitInformation::default();
        info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            &info as *const _ as *const c_void,
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
        ) == 0
        {
            warn!("设置 JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE 失败");
            return;
        }
        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            warn!("当前进程加入 Job Object 失败（可能已被其它 Job 管理）");
            return;
        }
        OK.store(true, Ordering::Relaxed);
    });
    OK.load(Ordering::Relaxed)
}

/// 仅供测试：当前进程是否已在某个 Job 中
#[cfg(test)]
fn is_current_process_in_job() -> bool {
    let mut result = 0;
    let ok = unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut result) };
    ok != 0 && result != 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_process_joins_kill_on_close_job() {
        assert!(super::setup_kill_on_close_job());
        assert!(super::is_current_process_in_job());
        // 重复调用应保持幂等
        assert!(super::setup_kill_on_close_job());
    }
}
