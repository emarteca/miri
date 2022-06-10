use rustc_middle::mir;
use rustc_span::Symbol;
use rustc_target::spec::abi::Abi;

use crate::*;
use shims::foreign_items::EmulateByNameResult;
use shims::unix::fs::EvalContextExt as _;
use shims::unix::linux::sync::futex;
use shims::unix::sync::EvalContextExt as _;
use shims::unix::thread::EvalContextExt as _;

impl<'mir, 'tcx: 'mir> EvalContextExt<'mir, 'tcx> for crate::MiriEvalContext<'mir, 'tcx> {}
pub trait EvalContextExt<'mir, 'tcx: 'mir>: crate::MiriEvalContextExt<'mir, 'tcx> {
    fn emulate_foreign_item_by_name(
        &mut self,
        link_name: Symbol,
        abi: Abi,
        args: &[OpTy<'tcx, Tag>],
        dest: &PlaceTy<'tcx, Tag>,
        _ret: mir::BasicBlock,
    ) -> InterpResult<'tcx, EmulateByNameResult<'mir, 'tcx>> {
        let this = self.eval_context_mut();

        match &*link_name.as_str() {
            // errno
            "__errno_location" => {
                let [] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let errno_place = this.last_error_place()?;
                this.write_scalar(errno_place.to_ref(this).to_scalar()?, dest)?;
            }

            // File related shims (but also see "syscall" below for statx)
            // These symbols have different names on Linux and macOS, which is the only reason they are not
            // in the `posix` module.
            "close" => {
                let [fd] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.close(fd)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "opendir" => {
                let [name] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.opendir(name)?;
                this.write_scalar(result, dest)?;
            }
            "readdir64" => {
                let [dirp] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.linux_readdir64(dirp)?;
                this.write_scalar(result, dest)?;
            }
            "ftruncate64" => {
                let [fd, length] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.ftruncate64(fd, length)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            // Linux-only
            "posix_fadvise" => {
                let [fd, offset, len, advice] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.read_scalar(fd)?.to_i32()?;
                this.read_scalar(offset)?.to_machine_isize(this)?;
                this.read_scalar(len)?.to_machine_isize(this)?;
                this.read_scalar(advice)?.to_i32()?;
                // fadvise is only informational, we can ignore it.
                this.write_null(dest)?;
            }
            "sync_file_range" => {
                let [fd, offset, nbytes, flags] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.sync_file_range(fd, offset, nbytes, flags)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }

            // Time related shims
            "clock_gettime" => {
                // This is a POSIX function but it has only been tested on linux.
                let [clk_id, tp] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.clock_gettime(clk_id, tp)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }

            // Querying system information
            "pthread_attr_getstack" => {
                // We don't support "pthread_attr_setstack", so we just pretend all stacks have the same values here.
                let [attr_place, addr_place, size_place] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.deref_operand(attr_place)?;
                let addr_place = this.deref_operand(addr_place)?;
                let size_place = this.deref_operand(size_place)?;

                this.write_scalar(
                    Scalar::from_uint(STACK_ADDR, this.pointer_size()),
                    &addr_place.into(),
                )?;
                this.write_scalar(
                    Scalar::from_uint(STACK_SIZE, this.pointer_size()),
                    &size_place.into(),
                )?;

                // Return success (`0`).
                this.write_null(dest)?;
            }

            // Threading
            "prctl" => {
                // prctl is variadic. (It is not documented like that in the manpage, but defined like that in the libc crate.)
                this.check_abi_and_shim_symbol_clash(abi, Abi::C { unwind: false }, link_name)?;
                let result = this.prctl(args)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "pthread_condattr_setclock" => {
                let [attr, clock_id] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.pthread_condattr_setclock(attr, clock_id)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }
            "pthread_condattr_getclock" => {
                let [attr, clock_id] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                let result = this.pthread_condattr_getclock(attr, clock_id)?;
                this.write_scalar(Scalar::from_i32(result), dest)?;
            }

            // Dynamically invoked syscalls
            "syscall" => {
                // We do not use `check_shim` here because `syscall` is variadic. The argument
                // count is checked bellow.
                this.check_abi_and_shim_symbol_clash(abi, Abi::C { unwind: false }, link_name)?;
                // The syscall variadic function is legal to call with more arguments than needed,
                // extra arguments are simply ignored. The important check is that when we use an
                // argument, we have to also check all arguments *before* it to ensure that they
                // have the right type.

                let sys_getrandom = this.eval_libc("SYS_getrandom")?.to_machine_usize(this)?;

                let sys_statx = this.eval_libc("SYS_statx")?.to_machine_usize(this)?;

                let sys_futex = this.eval_libc("SYS_futex")?.to_machine_usize(this)?;

                if args.is_empty() {
                    throw_ub_format!(
                        "incorrect number of arguments for syscall: got 0, expected at least 1"
                    );
                }
                match this.read_scalar(&args[0])?.to_machine_usize(this)? {
                    // `libc::syscall(NR_GETRANDOM, buf.as_mut_ptr(), buf.len(), GRND_NONBLOCK)`
                    // is called if a `HashMap` is created the regular way (e.g. HashMap<K, V>).
                    id if id == sys_getrandom => {
                        // The first argument is the syscall id, so skip over it.
                        if args.len() < 4 {
                            throw_ub_format!(
                                "incorrect number of arguments for `getrandom` syscall: got {}, expected at least 4",
                                args.len()
                            );
                        }
                        getrandom(this, &args[1], &args[2], &args[3], dest)?;
                    }
                    // `statx` is used by `libstd` to retrieve metadata information on `linux`
                    // instead of using `stat`,`lstat` or `fstat` as on `macos`.
                    id if id == sys_statx => {
                        // The first argument is the syscall id, so skip over it.
                        if args.len() < 6 {
                            throw_ub_format!(
                                "incorrect number of arguments for `statx` syscall: got {}, expected at least 6",
                                args.len()
                            );
                        }
                        let result =
                            this.linux_statx(&args[1], &args[2], &args[3], &args[4], &args[5])?;
                        this.write_scalar(Scalar::from_machine_isize(result.into(), this), dest)?;
                    }
                    // `futex` is used by some synchonization primitives.
                    id if id == sys_futex => {
                        futex(this, &args[1..], dest)?;
                    }
                    id => {
                        this.handle_unsupported(format!("can't execute syscall with ID {}", id))?;
                        return Ok(EmulateByNameResult::AlreadyJumped);
                    }
                }
            }

            // Miscelanneous
            "getrandom" => {
                let [ptr, len, flags] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                getrandom(this, ptr, len, flags, dest)?;
            }
            "sched_getaffinity" => {
                let [pid, cpusetsize, mask] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.read_scalar(pid)?.to_i32()?;
                this.read_scalar(cpusetsize)?.to_machine_usize(this)?;
                this.deref_operand(mask)?;
                // FIXME: we just return an error; `num_cpus` then falls back to `sysconf`.
                let einval = this.eval_libc("EINVAL")?;
                this.set_last_error(einval)?;
                this.write_scalar(Scalar::from_i32(-1), dest)?;
            }

            // Incomplete shims that we "stub out" just to get pre-main initialization code to work.
            // These shims are enabled only when the caller is in the standard library.
            "pthread_getattr_np" if this.frame_in_std() => {
                let [_thread, _attr] =
                    this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
                this.write_null(dest)?;
            }

            /*_ => {
                println!("NEVER CRASH");
                 this.write_null(dest)?;
            }*/

            _ => return Ok(EmulateByNameResult::NotSupported),
        };

        Ok(EmulateByNameResult::NeedsJumping)
    }
}

// Shims the linux `getrandom` syscall.
fn getrandom<'tcx>(
    this: &mut MiriEvalContext<'_, 'tcx>,
    ptr: &OpTy<'tcx, Tag>,
    len: &OpTy<'tcx, Tag>,
    flags: &OpTy<'tcx, Tag>,
    dest: &PlaceTy<'tcx, Tag>,
) -> InterpResult<'tcx> {
    let ptr = this.read_pointer(ptr)?;
    let len = this.read_scalar(len)?.to_machine_usize(this)?;

    // The only supported flags are GRND_RANDOM and GRND_NONBLOCK,
    // neither of which have any effect on our current PRNG.
    // See <https://github.com/rust-lang/rust/pull/79196> for a discussion of argument sizes.
    let _flags = this.read_scalar(flags)?.to_i32();

    this.gen_random(ptr, len)?;
    this.write_scalar(Scalar::from_machine_usize(len, this), dest)?;
    Ok(())
}
