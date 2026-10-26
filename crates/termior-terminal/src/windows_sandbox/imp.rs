//! Windows implementation of the AppContainer confinement primitives.

use super::{build_command_line, AppContainerError};
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, SetHandleInformation, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
    NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    GetLengthSid, ACL, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapAlloc, HeapFree, HEAP_ZERO_MEMORY};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// The access the container is granted on the shared directory.
///
/// This is `FILE_ALL_ACCESS` without `WRITE_DAC` and `WRITE_OWNER`. `FILE_ALL_ACCESS`
/// hands the container the right to rewrite the security descriptors of everything it
/// is given, which would let a confined process take ownership of the workspace and
/// change who else may enter it. The remaining rights are everything a task needs to
/// read, write and run inside the workspace.
const CONTAINER_ACCESS: u32 = 0x0013_01FF;

/// Environment variables the sandbox copies from the parent process.
///
/// The list is deliberately short. Anything that can carry a credential
/// (tokens, API keys, cloud config, SSH material) stays out of the sandbox.
const INHERITED_ENVIRONMENT: &[&str] = &[
    "SystemRoot",
    "windir",
    "ComSpec",
    "PATH",
    "PATHEXT",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "NUMBER_OF_PROCESSORS",
    "COMPUTERNAME",
    "SystemDrive",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "ALLUSERSPROFILE",
    "PUBLIC",
];

/// Environment variables that point at the sandbox home directory.
///
/// These are rewritten rather than inherited so a tool that insists on a home
/// directory writes inside the granted workspace instead of the real user
/// profile.
const HOME_ENVIRONMENT: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
];

/// Owns the AppContainer SID for the lifetime of a prepared environment.
pub struct AppContainer {
    sid: PSID,
}

unsafe impl Send for AppContainer {}

impl AppContainer {
    /// Resolve, creating if needed, the AppContainer profile SID.
    pub fn resolve(name: &str) -> Result<Self, AppContainerError> {
        let wide = wide_null(name);
        if let Some(sid) = derive_sid(&wide) {
            return Ok(Self { sid });
        }
        let mut created: PSID = std::ptr::null_mut();
        // SAFETY: the profile name, display name, and description are valid null-terminated
        // UTF-16 strings; no capabilities are requested, so the capability pointer is null
        // and the count is zero.
        let created_result = unsafe {
            CreateAppContainerProfile(
                wide.as_ptr(),
                wide.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                0,
                &mut created,
            )
        };
        if created_result >= 0 && !created.is_null() {
            return Ok(Self { sid: created });
        }
        // Another process may have created the profile between the derive and the create.
        if let Some(sid) = derive_sid(&wide) {
            return Ok(Self { sid });
        }
        Err(AppContainerError::Profile(format!(
            "cannot create the Windows AppContainer profile {name}: hr={created_result:#x}"
        )))
    }

    /// The resolved SID.
    pub fn sid(&self) -> PSID {
        self.sid
    }

    /// Grant the container full access to one directory.
    pub fn grant(&self, path: &Path) -> Result<(), AppContainerError> {
        grant_directory_to(path, self.sid)
    }

    /// Launch a command inside the container.
    pub fn spawn(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<ConfinedProcess, AppContainerError> {
        spawn_in_container(self, program, args, cwd)
    }

    /// Launch a command inside the container with piped standard streams.
    pub fn spawn_piped(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<PipedConfinedProcess, AppContainerError> {
        spawn_piped_in_container(self, program, args, cwd)
    }
}

/// Resolve the SID of an existing AppContainer profile.
///
/// `DeriveAppContainerSidFromAppContainerName` returns `S_OK` (0) on success and a
/// negative HRESULT on failure.
fn derive_sid(name: &[u16]) -> Option<PSID> {
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: `name` is a valid null-terminated UTF-16 string and `sid` is a valid
    // out-pointer for the duration of the call.
    let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    (derived >= 0 && !sid.is_null()).then_some(sid)
}

impl Drop for AppContainer {
    fn drop(&mut self) {
        // The profile is shared and long-lived, so it is intentionally not deleted.
        // Freeing the SID buffer is enough; the profile itself stays registered.
        if !self.sid.is_null() {
            // SAFETY: `self.sid` was allocated by the AppContainer API and is freed once.
            unsafe {
                LocalFree(self.sid);
            }
        }
    }
}

/// A process running inside the AppContainer.
pub struct ConfinedProcess {
    pid: u32,
    handle: HANDLE,
}

impl ConfinedProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Terminate the confined process.
    pub fn kill(&mut self) {
        // SAFETY: the handle is owned by this struct and open for the call.
        unsafe {
            TerminateProcess(self.handle, 1);
        }
    }

    /// Wait for the confined process to exit and return its exit code.
    pub fn wait(&mut self, timeout: std::time::Duration) -> Option<u32> {
        // SAFETY: the handle is owned by this struct and is open for the call.
        let result = unsafe {
            WaitForSingleObject(
                self.handle,
                timeout.as_millis().min(u32::MAX as u128) as u32,
            )
        };
        if result != 0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: the handle is open and `code` is a valid out-parameter.
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        if ok == 0 {
            None
        } else {
            Some(code)
        }
    }
}

impl Drop for ConfinedProcess {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateProcessW` and is closed exactly once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// A confined process whose standard streams are pipes owned by the parent.
pub struct PipedConfinedProcess {
    inner: ConfinedProcess,
    stdout: Option<File>,
    stderr: Option<File>,
    /// Kept alive so the job object stays open while the confined process runs.
    /// Dropping it closes the handle and kills every descendant.
    _job: Job,
}

impl PipedConfinedProcess {
    pub fn pid(&self) -> u32 {
        self.inner.pid()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    /// Wait for the confined process to exit and return its exit code.
    pub fn wait(&mut self, timeout: std::time::Duration) -> Option<u32> {
        self.inner.wait(timeout)
    }

    /// Terminate the confined process and its descendants.
    pub fn kill(&mut self) {
        self.inner.kill();
    }
}

impl Drop for PipedConfinedProcess {
    fn drop(&mut self) {
        self.inner.kill();
    }
}

/// A Windows Job Object that kills every process in its tree when the handle closes.
struct Job {
    handle: HANDLE,
}

impl Job {
    /// Create a job with `KILL_ON_JOB_CLOSE`.
    fn new() -> Result<Self, AppContainerError> {
        // SAFETY: a null name creates an unnamed job owned by this handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(AppContainerError::Spawn(format!(
                "cannot create the sandbox job object: os error {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the handle was just created and the information structure matches the
        // Windows layout for `JobObjectExtendedLimitInformation`.
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            // SAFETY: the handle is owned by this function and is closed exactly once.
            unsafe {
                CloseHandle(handle);
            }
            return Err(AppContainerError::Spawn(format!(
                "cannot configure the sandbox job object: os error {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self { handle })
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this struct and is closed exactly once.
        // Closing it terminates every process still in the job.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Grant the AppContainer SID full access to one directory, keeping the existing ACL.
///
/// The grant is *additive*. Writing a whole DACL with `SetNamedSecurityInfoW` would
/// drop every other principal's entry, which takes the host's own access away and
/// removes the traversal rights the ancestors need. `SetEntriesInAclW` merges the new
/// entry into the *current* DACL instead, so nothing else changes.
fn grant_directory_to(path: &Path, sid: PSID) -> Result<(), AppContainerError> {
    // SAFETY: `sid` is a valid SID.
    let length = unsafe { GetLengthSid(sid) };
    if length == 0 {
        return Err(AppContainerError::Profile(
            "invalid AppContainer SID".into(),
        ));
    }
    // The trustee carries the SID itself. `TRUSTEE_IS_SID` means `ptstrName` points
    // at a `PSID`, not at a name, so the raw SID goes there.
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_USER,
        ptstrName: sid as *mut u16,
    };
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: CONTAINER_ACCESS,
        // `SET_ACCESS` replaces the container's own entry instead of stacking a new
        // one on top of it, so granting the same directory twice does not grow the
        // DACL.
        grfAccessMode: SET_ACCESS,
        // Only a directory can pass its access down to the files below it. A file
        // grant applies to that file alone.
        grfInheritance: if path.is_dir() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            windows_sys::Win32::Security::NO_INHERITANCE
        },
        Trustee: trustee,
    };
    // The current DACL is the base for the merge. Building the new ACL without it
    // would produce a one-entry ACL that replaces everything else on the path.
    let wide = wide_null(&path.to_string_lossy());
    let mut existing: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` is a valid null-terminated path and both out-parameters are
    // valid for the duration of the call.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut existing,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(AppContainerError::Acl(format!(
            "cannot read the ACL of {}: status={status:#x}",
            path.display()
        )));
    }
    let mut merged: *mut ACL = std::ptr::null_mut();
    // SAFETY: `entry` is a valid explicit access entry for one trustee, and the
    // trustee points at the live SID, which outlives the call.
    let status = unsafe {
        SetEntriesInAclW(
            1,
            &entry as *const EXPLICIT_ACCESS_W,
            existing,
            &mut merged as *mut *mut ACL,
        )
    };
    // SAFETY: the DACL and the security descriptor were allocated by
    // `GetNamedSecurityInfoW`. The DACL pointer points *into* the security
    // descriptor buffer, so only the descriptor itself is freed.
    unsafe {
        if !descriptor.is_null() {
            LocalFree(descriptor);
        }
    }
    if status != 0 || merged.is_null() {
        return Err(AppContainerError::Acl(format!(
            "cannot build the AppContainer grant for {}: status={status:#x}",
            path.display()
        )));
    }
    // SAFETY: `wide` is a valid null-terminated path and `merged` is the ACL
    // `SetEntriesInAclW` just built.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            windows_sys::Win32::Security::DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            merged,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: the ACL was allocated by `SetEntriesInAclW` and is freed exactly once.
    unsafe {
        LocalFree(merged as *mut std::ffi::c_void);
    }
    if status != 0 {
        return Err(AppContainerError::Acl(format!(
            "cannot grant the AppContainer access to {}: status={status:#x}",
            path.display()
        )));
    }
    Ok(())
}

/// Launch a command inside the AppContainer.
fn spawn_in_container(
    container: &AppContainer,
    program: &Path,
    args: &[String],
    cwd: &Path,
) -> Result<ConfinedProcess, AppContainerError> {
    let mut attribute_list = AttributeList::new(1)?;
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: container.sid(),
        CapabilityCount: 0,
        Capabilities: std::ptr::null_mut(),
        Reserved: 0,
    };
    // SAFETY: `attribute_list` is initialized and `capabilities` outlives the call.
    let updated = unsafe {
        UpdateProcThreadAttribute(
            attribute_list.pointer(),
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            &capabilities as *const SECURITY_CAPABILITIES as *const std::ffi::c_void,
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if updated == 0 {
        return Err(AppContainerError::Spawn(
            "cannot set the AppContainer security capabilities".into(),
        ));
    }
    let mut startup = STARTUPINFOEXW {
        StartupInfo: unsafe { std::mem::zeroed() },
        lpAttributeList: attribute_list.pointer(),
    };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    let mut command_line = wide_null(&build_command_line(program, args));
    let wide_cwd = wide_null(&cwd.to_string_lossy());
    // An explicit environment block. Passing null would inherit the parent's
    // environment, which leaks credentials into the sandbox. Windows itself needs a few
    // variables to start a process, so those are provided and nothing else is.
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let entries = build_environment_block(&system_root, cwd);
    let mut environment: Vec<u16> = Vec::new();
    for entry in &entries {
        environment.extend(entry.encode_utf16());
        environment.push(0);
    }
    // The block is a sequence of null-terminated strings terminated by an extra null.
    // `CreateProcessW` reads exactly that shape.
    environment.push(0);
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the attribute list is initialized with one entry, the command line and
    // working directory are valid null-terminated strings, and `information` is a valid
    // out-parameter.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr() as *const std::ffi::c_void,
            wide_cwd.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    };
    // SAFETY: the list is no longer needed by the child process.
    if created == 0 {
        return Err(AppContainerError::Spawn(format!(
            "cannot start the sandboxed command: os error {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(ConfinedProcess {
        pid: information.dwProcessId,
        handle: information.hProcess,
    })
}

/// Launch a command inside the container with piped standard streams.
///
/// `CreateProcessW` needs inheritable handles for the standard streams, and the parent
/// side of each pipe must not be inherited by the child, otherwise the pipe would never
/// report end-of-file.
fn spawn_piped_in_container(
    container: &AppContainer,
    program: &Path,
    args: &[String],
    cwd: &Path,
) -> Result<PipedConfinedProcess, AppContainerError> {
    // The child end of stdout and stderr is the write end; for stdin it is the read end.
    let mut stdout = create_pipe(PipeDirection::ChildWrites)?;
    let mut stderr = create_pipe(PipeDirection::ChildWrites)?;
    let mut stdin = create_pipe(PipeDirection::ChildReads)?;

    // Only these three handles may cross into the child. Everything else in the parent
    // stays unreachable from inside the container.
    let handle_list = [stdin.child, stdout.child, stderr.child];

    // The job object bounds the process tree. Closing it kills every descendant.
    let job = Job::new()?;
    let job_list = [job.handle];

    let mut attribute_list = AttributeList::new(3)?;
    {
        let capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: container.sid(),
            CapabilityCount: 0,
            Capabilities: std::ptr::null_mut(),
            Reserved: 0,
        };
        // SAFETY: `attribute_list` is initialized with two entries and `capabilities`
        // outlives the call.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attribute_list.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                &capabilities as *const SECURITY_CAPABILITIES as *const std::ffi::c_void,
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(AppContainerError::Spawn(
                "cannot set the AppContainer security capabilities".into(),
            ));
        }
        // SAFETY: `attribute_list` is initialized and `handle_list` is a valid array of
        // open inheritable handles that outlives the call.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attribute_list.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handle_list.as_ptr() as *const std::ffi::c_void,
                std::mem::size_of_val(&handle_list),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(AppContainerError::Spawn(format!(
                "cannot set the inherited handle list: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: `attribute_list` is initialized and `job_list` is a valid array of
        // job handles that outlives the call.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                attribute_list.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                job_list.as_ptr() as *const std::ffi::c_void,
                std::mem::size_of_val(&job_list),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(AppContainerError::Spawn(format!(
                "cannot assign the sandbox job object: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    let mut startup = STARTUPINFOEXW {
        StartupInfo: unsafe { std::mem::zeroed() },
        lpAttributeList: attribute_list.pointer(),
    };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.child;
    startup.StartupInfo.hStdOutput = stdout.child;
    startup.StartupInfo.hStdError = stderr.child;

    let mut command_line = wide_null(&build_command_line(program, args));
    let wide_cwd = wide_null(&cwd.to_string_lossy());
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let entries = build_environment_block(&system_root, cwd);
    let mut environment: Vec<u16> = Vec::new();
    for entry in &entries {
        environment.extend(entry.encode_utf16());
        environment.push(0);
    }
    environment.push(0);

    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the attribute list is initialized with two entries, the command line and
    // working directory are valid null-terminated strings, the handle list contains
    // exactly the three inheritable standard-stream handles, and `information` is a
    // valid out-parameter.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr() as *const std::ffi::c_void,
            wide_cwd.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    };
    if created == 0 {
        return Err(AppContainerError::Spawn(format!(
            "cannot start the sandboxed command: os error {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: the thread handle came from `CreateProcessW` and is closed exactly once.
    unsafe {
        CloseHandle(information.hThread);
    }
    // The parent must drop its copies of the child ends. Otherwise the write end stays
    // open in this process and the reader never sees end-of-file.
    stdout.release_child();
    stderr.release_child();
    stdin.release_child();
    // The parent ends of the pipes now belong to the returned process.
    let stdout_parent = stdout.parent;
    let stderr_parent = stderr.parent;
    stdout.parent = None;
    stderr.parent = None;
    stdin.parent = None;
    Ok(PipedConfinedProcess {
        inner: ConfinedProcess {
            pid: information.dwProcessId,
            handle: information.hProcess,
        },
        stdout: stdout_parent.map(|handle| {
            // SAFETY: the handle was created by `CreatePipe` and is not owned elsewhere.
            unsafe { File::from_raw_handle(handle.cast()) }
        }),
        stderr: stderr_parent.map(|handle| {
            // SAFETY: the handle was created by `CreatePipe` and is not owned elsewhere.
            unsafe { File::from_raw_handle(handle.cast()) }
        }),
        _job: job,
    })
}

/// Which end of a pipe the child process owns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PipeDirection {
    /// The child reads from the pipe, so the child owns the read end.
    ChildReads,
    /// The child writes to the pipe, so the child owns the write end.
    ChildWrites,
}

/// Create a pipe whose child end is inheritable and whose parent end is not.
fn create_pipe(direction: PipeDirection) -> Result<Pipe, AppContainerError> {
    use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-pointers are valid for the call and the attribute pointer is
    // null, so the pipes get the default security descriptor.
    let created = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) };
    if created == 0 {
        return Err(AppContainerError::Spawn(format!(
            "cannot create a sandbox pipe: os error {}",
            std::io::Error::last_os_error()
        )));
    }
    let (parent, child) = match direction {
        PipeDirection::ChildReads => (write, read),
        PipeDirection::ChildWrites => (read, write),
    };
    // The child end must be inheritable; the parent end must not be, otherwise the
    // child keeps the read side open and the parent never sees end-of-file.
    // SAFETY: both handles are open and owned by this function.
    unsafe {
        if SetHandleInformation(child, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
            let error = std::io::Error::last_os_error();
            CloseHandle(read);
            CloseHandle(write);
            return Err(AppContainerError::Spawn(format!(
                "cannot mark the sandbox pipe inheritable: {error}"
            )));
        }
        if SetHandleInformation(parent, HANDLE_FLAG_INHERIT, 0) == 0 {
            let error = std::io::Error::last_os_error();
            CloseHandle(read);
            CloseHandle(write);
            return Err(AppContainerError::Spawn(format!(
                "cannot clear the sandbox pipe inherit flag: {error}"
            )));
        }
    }
    Ok(Pipe {
        parent: Some(parent),
        child,
    })
}

/// One pipe: a parent end and an inheritable child end.
struct Pipe {
    parent: Option<HANDLE>,
    child: HANDLE,
}

impl Pipe {
    /// Close the parent copy of the child end. The child process keeps its own copy.
    fn release_child(&mut self) {
        if !self.child.is_null() {
            // SAFETY: the handle is owned by this struct and closed exactly once.
            unsafe {
                CloseHandle(self.child);
            }
            self.child = std::ptr::null_mut();
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: each handle is closed exactly once.
        unsafe {
            if let Some(parent) = self.parent.take() {
                CloseHandle(parent);
            }
            if !self.child.is_null() {
                CloseHandle(self.child);
            }
        }
    }
}

/// A `PROC_THREAD_ATTRIBUTE_LIST` that frees itself.
struct AttributeList {
    pointer: *mut std::ffi::c_void,
}

impl AttributeList {
    fn new(count: u32) -> Result<Self, AppContainerError> {
        let mut size = 0usize;
        // SAFETY: passing a null list asks the API for the required buffer size.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(AppContainerError::Spawn(
                "cannot size the process attribute list".into(),
            ));
        }
        // SAFETY: `size` is the byte count the API just reported.
        let list = unsafe { HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, size) };
        if list.is_null() {
            return Err(AppContainerError::Spawn(
                "cannot allocate the process attribute list".into(),
            ));
        }
        // SAFETY: `list` has the size the API requested.
        let initialized = unsafe { InitializeProcThreadAttributeList(list, count, 0, &mut size) };
        if initialized == 0 {
            // SAFETY: `list` was allocated above and is freed exactly once.
            unsafe {
                HeapFree(GetProcessHeap(), 0, list as *const std::ffi::c_void);
            }
            return Err(AppContainerError::Spawn(
                "cannot initialize the process attribute list".into(),
            ));
        }
        Ok(Self { pointer: list })
    }

    fn pointer(&mut self) -> *mut std::ffi::c_void {
        self.pointer
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialized by `AttributeList::new` and is freed once.
        unsafe {
            DeleteProcThreadAttributeList(self.pointer);
            HeapFree(GetProcessHeap(), 0, self.pointer as *const std::ffi::c_void);
        }
    }
}

/// Delete an AppContainer profile by name.
pub fn delete_profile(name: &str) {
    let wide = wide_null(name);
    // SAFETY: `wide` is a valid null-terminated profile name.
    unsafe {
        DeleteAppContainerProfile(wide.as_ptr());
    }
}

/// Build the environment block for a confined process.
///
/// Windows refuses to start an AppContainer process unless the block contains
/// `LOCALAPPDATA` (error 203, `ERROR_ENVVAR_NOT_FOUND`), so that variable is always
/// present. Everything else is copied from the parent through a short allowlist, and
/// every home-like variable is redirected into the granted workspace so a tool that
/// insists on a home directory writes inside the sandbox instead of the real profile.
fn build_environment_block(system_root: &str, home: &Path) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for name in INHERITED_ENVIRONMENT {
        if let Ok(value) = std::env::var(name) {
            if value.is_empty() {
                continue;
            }
            entries.push(format!("{name}={value}"));
        }
    }
    // The sandbox needs a shell and the system tools even when the parent process
    // was started with a stripped environment.
    if !entries.iter().any(|entry| entry.starts_with("SystemRoot=")) {
        entries.push(format!("SystemRoot={system_root}"));
    }
    if !entries.iter().any(|entry| entry.starts_with("windir=")) {
        entries.push(format!("windir={system_root}"));
    }
    if !entries.iter().any(|entry| entry.starts_with("ComSpec=")) {
        entries.push(format!("ComSpec={system_root}\\System32\\cmd.exe"));
    }
    if !entries.iter().any(|entry| entry.starts_with("PATH=")) {
        entries.push(format!(
            "PATH={system_root}\\System32;{system_root};{system_root}\\System32\\Wbem"
        ));
    }
    if !entries.iter().any(|entry| entry.starts_with("OS=")) {
        entries.push("OS=Windows_NT".to_string());
    }
    if !entries.iter().any(|entry| entry.starts_with("PATHEXT=")) {
        entries.push("PATHEXT=.COM;.EXE;.BAT;.CMD".to_string());
    }
    let home = home.display().to_string();
    for name in HOME_ENVIRONMENT {
        entries.push(format!("{name}={home}"));
    }
    entries.sort_by_key(|entry| {
        entry
            .split('=')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    });
    entries
}

/// Encode a string as a null-terminated UTF-16 buffer.
pub fn wide_null(text: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = std::ffi::OsStr::new(text).encode_wide().collect();
    wide.push(0);
    wide
}
