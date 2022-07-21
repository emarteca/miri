use rustc_span::Symbol;
use rustc_target::spec::abi::Abi;

use crate::*;
use shims::foreign_items::EmulateByNameResult;
use shims::unix::fs::EvalContextExt as _;
use shims::unix::thread::EvalContextExt as _;

impl<'mir, 'tcx: 'mir> EvalContextExt<'mir, 'tcx> for crate::MiriEvalContext<'mir, 'tcx> {}
pub trait EvalContextExt<'mir, 'tcx: 'mir>: crate::MiriEvalContextExt<'mir, 'tcx> {
    fn emulate_foreign_item_by_name(
        &mut self,
        link_name: Symbol,
        abi: Abi,
        args: &[OpTy<'tcx, Provenance>],
        dest: &PlaceTy<'tcx, Provenance>,
    ) -> InterpResult<'tcx, EmulateByNameResult<'mir, 'tcx>> {
        let this = self.eval_context_mut();

        match link_name.as_str() {
            // errno
            "__error" => {
                let [] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let errno_place = this.last_error_place()?;
                this.write_scalar(errno_place.to_ref(this).to_scalar()?, dest)?;
            }

            // File related shims
            "close$NOCANCEL" => {
                let [result] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.close(result)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "stat" | "stat64" | "stat$INODE64" => {
                let [path, buf] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.macos_stat(path, buf)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "lstat" | "lstat64" | "lstat$INODE64" => {
                let [path, buf] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.macos_lstat(path, buf)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "fstat" | "fstat64" | "fstat$INODE64" => {
                let [fd, buf] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.macos_fstat(fd, buf)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "opendir$INODE64" => {
                let [name] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.opendir(name)?;
                this.write_scalar(result, dest)?;
            }
            "readdir_r" | "readdir_r$INODE64" => {
                let [dirp, entry, result] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.macos_readdir_r(dirp, entry, result)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "lseek" => {
                let [fd, offset, whence] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                // macOS is 64bit-only, so this is lseek64
                let result = this.lseek64(fd, offset, whence)?;
                this.write_scalar(Scalar::from_i64(result), dest)?;
            }
            "ftruncate" => {
                let [fd, length] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                // macOS is 64bit-only, so this is ftruncate64
                let result = this.ftruncate64(fd, length)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }

            // Environment related shims
            "_NSGetEnviron" => {
                let [] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.write_pointer(
                    this.machine.env_vars.environ.expect("machine must be initialized").ptr,
                    dest,
                )?;
            }

            // Time related shims
            "mach_absolute_time" => {
                let [] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.mach_absolute_time()?;
                this.write_scalar(Scalar::from_u64(result), dest)?;
            }

            "mach_timebase_info" => {
                let [info] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.mach_timebase_info(info)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }

            // Access to command-line arguments
            "_NSGetArgc" => {
                let [] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.write_pointer(
                    this.machine.argc.expect("machine must be initialized").ptr,
                    dest,
                )?;
            }
            "_NSGetArgv" => {
                let [] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.write_pointer(
                    this.machine.argv.expect("machine must be initialized").ptr,
                    dest,
                )?;
            }

            // Thread-local storage
            "_tlv_atexit" => {
                let [dtor, data] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let dtor = this.read_pointer(dtor)?;
                let dtor = this.get_ptr_fn(dtor)?.as_instance()?;
                let data = this.read_scalar(data)?.check_init()?;
                let active_thread = this.get_active_thread();
                this.machine.tls.set_macos_thread_dtor(active_thread, dtor, data)?;
            }

            // Querying system information
            "pthread_get_stackaddr_np" => {
                let [thread] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.read_scalar(thread)?.to_machine_usize(this)?;
                let stack_addr = Scalar::from_uint(STACK_ADDR, this.pointer_size());
                this.write_scalar(stack_addr, dest)?;
            }
            "pthread_get_stacksize_np" => {
                let [thread] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.read_scalar(thread)?.to_machine_usize(this)?;
                let stack_size = Scalar::from_uint(STACK_SIZE, this.pointer_size());
                this.write_scalar(stack_size, dest)?;
            }

            // Threading
            "pthread_setname_np" => {
                let [name] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let name = this.read_pointer(name)?;
                this.pthread_setname_np(name)?;
            }

            // Incomplete shims that we "stub out" just to get pre-main initialization code to work.
            // These shims are enabled only when the caller is in the standard library.
            "mmap" if this.frame_in_std() => {
                // This is a horrible hack, but since the guard page mechanism calls mmap and expects a particular return value, we just give it that value.
                let [addr, _, _, _, _, _] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let addr = this.read_scalar(addr)?.check_init()?;
                this.write_scalar(addr, dest)?;
            }

            _ => return Ok(EmulateByNameResult::NotSupported),
        };

        Ok(EmulateByNameResult::NeedsJumping)
    }
}
