# Miri C FFI Extension

This doc describes a proposed design for extending Miri to support the Rust C FFI. 

## Relevant links
* [Full corresponding design doc](https://docs.google.com/document/d/1JuvORO1_EtYBH1IsjJroxIMbWAgSGXOWvRiQyvF443I/edit?usp=sharing).
* [Fork of Miri](https://github.com/emarteca/miri)
* [Pull request for support of C functions with `int` args/returns](https://github.com/rust-lang/miri/pull/2363)
* [Linked issue](https://github.com/rust-lang/miri/issues/2365)

## Miri Design
At its core, Miri is an [abstract machine](https://github.com/rust-lang/miri/blob/master/src/machine.rs). It represents the state of the program it is executing, including an internal model of the memory used by the process.  It consists of an `Evaluator` struct, that has fields for all the components of the runtime state.

### The Abstract Machine
The Rust compiler provides a `Machine` trait designed to help instantiate an interpreter for [MIR](https://doc.rust-lang.org/nightly/nightly-rustc/src/rustc_const_eval/interpret/machine.rs.html). It provides hooks for different operations involved in program execution. For example, the trait provides a hook function `memory_read` which is used to add custom functionality to memory reads. The code for this hook stub in the rustc `Machine` is as follows:
```[rust]
/// Hook for performing extra checks on a memory read access.
///
/// Takes read-only access to the allocation so we can keep all the memory read
/// operations take `&self`. Use a `RefCell` in `AllocExtra` if you
/// need to mutate.
#[inline(always)]
fn memory_read(
    _tcx: TyCtxt<'tcx>,
    _machine: &Self,
    _alloc_extra: &Self::AllocExtra,
    _tag: (AllocId, Self::TagExtra),
    _range: AllocRange,
) -> InterpResult<'tcx> {
    Ok(())
}
```


In Miri, the `Evaluator` implements the rustc [`Machine` trait](https://github.com/rust-lang/miri/blob/master/src/machine.rs#L488). There, it overrides some trait functions. Among these functions is the [`memory_read` function](https://github.com/rust-lang/miri/blob/master/src/machine.rs#L745) described above: here, Miri has some custom functionality for tracking and dealing with data races and stacked borrows when memory is accessed.

### The Evaluation Context
The `Machine` is running inside an evaluation context. This is the [`InterpCx` (Interpreter Context) struct](https://doc.rust-lang.org/nightly/nightly-rustc/src/rustc_const_eval/interpret/eval_context.rs.html#31), provided again by the rustc interpreter support. Miri has its own version of the `InterpCx`, the [`MiriEvalContext`](https://github.com/rust-lang/miri/blob/master/src/machine.rs#L469), which is just the base `InterpCx` with the appropriate lifetime parameters, for the Miri Evaluator.
```[rust]
/// A rustc InterpCx for Miri.
pub type MiriEvalContext<'mir, 'tcx> = InterpCx<'mir, 'tcx, Evaluator<'mir, 'tcx>>;
```


Miri also provides an extension trait for custom evaluation contexts even within Miri itself. This is the mechanism by which different parts of Miri modularize their customizations to the environment. For example, Miri provides some [functionality for detecting data races](https://github.com/rust-lang/miri/blob/master/src/concurrency/data_race.rs).  As part of this functionality, they extend the evaluation context with some data race-specific functions: this is done by extending the `MiriEvalContext`.

### Current FFI Support
Miri does currently have some limited support for foreign function calls via emulation. This is all contained in the [`foreign_items` module](https://github.com/rust-lang/miri/blob/master/src/shims/foreign_items.rs).

This support consists of a hardcoded list of manually emulated functions, built to support commonly used foreign functions such as `malloc`. As it stands, there is a custom extension to the `MiriEvalContext` ([in shims/mod](https://github.com/rust-lang/miri/blob/master/src/shims/mod.rs#L25)) that implements a custom hook for function calls. This hook calls a function `emulate_foreign_item` if the function being called is identified as being a “foreign item” (i.e., if its body cannot be found). The relevant call, along with the corresponding comments, is included below to illustrate.
```[rust]
// Try to see if we can do something about foreign items.
if this.tcx.is_foreign_item(instance.def_id()) {
    // An external function call that does not have a MIR body. We either find MIR elsewhere
    // or emulate its effect.
    // This will be Ok(None) if we're emulating the intrinsic entirely within Miri (no need
    // to run extra MIR), and Ok(Some(body)) if we found MIR to run for the
    // foreign function
    // Any needed call to `goto_block` will be performed by `emulate_foreign_item`.
    return this.emulate_foreign_item(instance.def_id(), abi, args, dest, ret, unwind);
}
```

Since this list of supported foreign functions is hardcoded, it is limited to only built-in native calls (and is not an exhaustive list of these). If Miri encounters a foreign item whose name is unknown, then it throws an unsupported exception and crashes the interpreter.

## Proposed plan

We plan to make all modifications to Miri itself, and not touch the C code being executed. This will be done with hooks around calls to external functions: data returned from C calls or passed as arguments to C calls could be wrapped in “guard” or “wrapper” structs that track their representation in each language and handle the memory synchronization between C and Miri when appropriate.

### Calling C from Miri
In order to call C code from a Rust program executing in Miri, we are extending Miri with the [`libffi`](https://docs.rs/libffi/latest/libffi/index.html) crate. This provides an interface to the host system’s libffi. It allows us to dispatch calls to linked code.

#### Linking C code
Miri doesn’t currently have a mechanism to link to external C code. We’ve implemented this by adding a new command line argument `-Zmiri-external_c_so_file` that users can use to specify a path to a shared object file. 

#### Dispatching calls to C code
When an external C call is encountered by Miri, the steps it follows to dispatch the call are:
1. Load the specified linked C shared object file (if applicable)
    a. using the [`libloading`](https://docs.rs/libloading/0.7.3/libloading/) crate
2. Load the specified function call from the linked library
    a. again using `libloading`
3. Convert all arguments to the function into values that can be passed into C
4. Call the C function
    a. using [`libffi`’s `call` function](https://docs.rs/libffi/latest/libffi/high/call/fn.call.html)
5. Store the return value of the function

Following is a simplified example of the code we added to call a function that returns an `i32` primitive using `libffi`. Note that we’ve removed the error handling code for simplicity. 

```[rust]
unsafe {
   let lib = libloading::Library::new(this.machine.external_c_so_file.as_ref());
   let func: libloading::Symbol<unsafe extern fn()> = 
                                       lib.get(link_name.as_str().as_bytes());

   // get the code pointer 
   let ptr = CodePtr(*func.deref() as *mut _);
        
   // call function and get return value (in this case an i32)
   let x = call::<i32>(ptr, &libffi_args.as_slice()); 
        
   // store the value in Miri's internal memory …
}
```

[`CodePtr`](https://docs.rs/libffi/latest/libffi/low/struct.CodePtr.html) is a code pointer type supplied by `libffi`, to provide access to the function being called.

#### A note on types
Part of the simplification of the code above is that it elides the type conversion required to turn values from their Miri representation into their corresponding values that get passed into the C function call. This is required for both the function arguments (to construct the `libffi_args` vector, we iterate over the arguments to the call in Miri) and for the function return. To determine the types that the arguments and return are expected to have, we need to extract the corresponding function signature. We assume that every C function called will have a corresponding signature defined in an `extern C` block earlier in the Rust program, which are represented in Miri as `ForeignItem`s (specifically, [`ForeignItem`s where the `ForeignItemKind` is a function](https://doc.rust-lang.org/beta/nightly-rustc/src/rustc_hir/hir.rs.html#3156)). We extract these signatures by parsing the `ForeignItem`s, and store them in a struct we built:
```[rust]
/// Representation of the function signature of an external C function.
/// Stores the function name, and its input (i.e., argument) and output (i.e., return) types
pub struct ExternalCFuncDeclRep<'hir> {
    /// Name of the function
    pub link_name: Symbol,
    /// Array of argument types
    pub inputs_types: &'hir [Ty<'hir>],
    /// Return type
    pub output_type: &'hir FnRetTy<'hir>,
}
```

To determine a correspondence between the Miri types and the C types, we refer to the available Miri types ([`TyKind`](https://doc.rust-lang.org/beta/nightly-rustc/src/rustc_hir/hir.rs.html#2522)s) and the types that implement the [`CType` trait](https://docs.rs/libffi/latest/libffi/high/types/trait.CType.html) in `libffi`. Clearly these do not have a 1:1 correspondence: there are many more complex types with a Miri `TyKind` representation that are not explicitly supported by `CType`. For us to support these types, we will need to make use of the “catch-all” CTypes: `*const T` and `*mut T`, the pointers. This will involve ensuring that the memory layout of the value is consistent with the type that both C and Rust expect, and we’re still figuring out how this is going to work.

As a demonstrative example of this conversion code, here is the code for converting a list of arguments that are all i32.
```[rust]
// get the function arguments
let mut libffi_args = Vec::<(Box<dyn Any>, CArg)>::with_capacity(args.len());
for (cur_arg, arg_type) in     
         args.iter().zip(external_fct_defn.inputs_types.iter()) {
     match this.read_scalar(cur_arg) {
        Ok(k) => {
            // the ints
            if let (Ok(v), &hir::Ty{
                hir_id:_, kind: hir::TyKind::Path(
                    hir::QPath::Resolved(_, hir::Path { 
                        span: _, 
                        res: hir::def::Res::PrimTy(hir::PrimTy::Int(IntTy::I32)), ..},..)
                    ), ..
            }) = (k.to_i8(), arg_type) {
                    libffi_args.push((Box::new(v), CArg::Int32(v)));
        },
// ...
```

The code for getting the corresponding return type is similar, just matching over the `external_fct_defn.output_type` instead of the `input_types`.


#### When and where are we dispatching the C calls?
As discussed above, Miri handles dispatching to its emulated foreign functions through a function called `emulate_foreign_item_by_name` in the `foreign_items` module. In our implementation, we are adding the dispatch to linked foreign functions before the match to try and call the built-in emulated functions. 

The effect of this decision is that now, if Miri encounters a call to a linked foreign function that has the same name as a built-in (emulated) function, then the linked implementation will be run instead of the emulated version. If there is no linked foreign function then the execution will proceed as before: Miri will check to see if the foreign function matches one that is emulated, and if not, it will throw an unsupported error. The reasoning behind this design decision is that if a developer provides a function that has the same signature as a built-in function, it will take precedence over the built-in function, and we want to model this behavior. 

### Modification of the Miri memory model
The rustc Machine provides a hook for adding “extra” information when memory is allocated, in the form of an [`AllocExtra` struct](https://doc.rust-lang.org/nightly/nightly-rustc/src/rustc_const_eval/interpret/machine.rs.html#485). The hooks called when memory is accessed (`memory_read`, `memory_written`, and `memory_deallocated`) all take an instance of `AllocExtra` as an argument. This is the mechanism by which `Machine` implementations can add tags on particular types of allocations and trigger behavior dependent on that tag. 

Miri has a list of kinds of memory, [`MiriMemoryKind`](https://github.com/rust-lang/miri/blob/master/src/machine.rs#L72). For example, in the [emulation of `malloc`](https://github.com/rust-lang/miri/blob/master/src/shims/foreign_items.rs#L399), Miri tags the memory as `MiriMemoryKind::C`. For example, here is the code that emulates malloc:
```[rust]
"malloc" => {
    let [size] = this.check_shim(abi, Abi::C { unwind: false }, link_name, args)?;
    let size = this.read_scalar(size)?.to_machine_usize(this)?;
    let res = this.malloc(size, /*zero_init:*/ false, MiriMemoryKind::C)?;
    this.write_pointer(res, dest)?;
}
```

We have added a new type of memory, `CInternal`, that we use to tag memory that corresponds to the return from a call to external C. This tagging is required so that we can differentiate pointers to C memory from pointers to Miri memory, since these need to be read differently. Further discussion on how to read C memory is in the section on Delayed Memory Allocation.

Miri has a struct `AllocExtra` that stores information on data races and other types of properties relevant on memory access. We have extended this with information on the C memory. This is required because the memory tag is not available in the `Machine` memory access hooks: instead, in [`init_allocation_extra`](https://doc.rust-lang.org/nightly/nightly-rustc/src/rustc_const_eval/interpret/machine.rs.html#339), the `AllocExtra` struct gets the C memory information added if the memory tag specifies that it is `CInternal`.

#### Delayed memory allocation
If a call to an external C function returns a pointer, we don’t know the shape of the object it points to until it is used later. For example, if a C function returns `*i32`, this could reference the head of an array of arbitrary length. 

We are planning to delay the allocation of MIRI memory corresponding to the C pointers until they are used (i.e., read); we could deduce the size/type of the data in the pointer from the context in which it is used.
To do this, we will need to intercept memory reads and recognize when we are reading memory tagged `CInternal`.

To implement this delayed memory allocation, we create a struct `CPointerWrapper` that stores a pointer returned from a call to an external C function. We have added a `GlobalState` to the `Evaluator` that is just a hashmap of these pointer wrappers. Then, at the Rust program points where the C pointer is used, it will be dereferenced through a method on the wrapper which accesses the corresponding C memory. This will allow the wrapper to act as an interface that will handle any operations (such as writing to the pointer, or pointer arithmetic to access other pointers) done to the pointer, by converting these operations into their corresponding operations in C. 

This access through the pointer wrapper object means we avoid writing the C memory that is being read from Rust into the MIRI memory, and then reading the C pointer access from the MIRI memory. The advantages of this is that it avoids the following potential issues:
* Relationships between C pointers not being preserved because their allocation in MIRI memory is not at the same offset as in the C memory
    * or, conversely, (arithmetic) relationships between C pointers being created which do not exist in the C memory
* MIRI representations of C memory needing to be reallocated/resized because a later usage in the Rust program reveals that a larger buffer was needed
* Needing to synchronize the MIRI and C representations of the same (shared) memory locations before and after calls to C functions (see the section on Synchronizing Memory)

This being said, we will likely still sometimes need to write C memory into MIRI memory so it can be used natively by the Rust program, or vice versa. One particular instance of this is if a C pointer is cast to a Rust struct. There is also the case of Rust pointers being later passed to C functions. In these cases, the above-mentioned issues on memory synchronization will become relevant (see the section on Synchronizing Memory).

### Technical Challenges
We have run into one main challenge in our implementation of this solution so far: how to intercept the reads to C pointers in MIRI so that we can trigger the relevant `CPointerWrapper` calls and return the correct values.

#### Current approach
Our current approach is not ideal, as it involves some MIRI memory allocation for every C pointer returned. Currently, when a C pointer is returned we dereference it and write \<currently just the type it is returned as\> it to memory, but we tag this memory as CInternal with the key of the corresponding pointer wrapper. The goal of this is to act as a trigger so that when MIRI accesses that memory, it is directed to the relevant CPointerWrapper.

For example, this is the code for doing this pointer wrapping for `*i32`.
```[rust]
let ret_ptr = call::<*mut i32>(ptr, &libffi_args.as_slice());
let ret_ptr_internal_wrapper = CPointerWrapper::Mutable(MutableCPointerWrapper::I32(ret_ptr));
let ptr_id = this.machine.foreign_items.borrow_mut().add_internal_C_pointer_wrapper(ret_ptr_internal_wrapper);
// read the value from the pointer and store it in mem
let c_i32 = *ret_ptr;
let res = this.malloc_value(/* ne == native endian */ &c_i32.to_ne_bytes(), MiriMemoryKind::CInternal(ptr_id))?;
this.write_pointer(res, dest)?;
```

When memory with a `CInternal` tag is accessed, we access the corresponding C memory to extract the relevant value. This is working, however, we are not currently returning the C value from this read to the MIRI program. 

Following is a diagram representing this memory indirection.
![](https://i.imgur.com/3RKaR58.png)

[We have intercepted the reads of C pointers from the Rust side](https://github.com/emarteca/miri/blob/4c44e920edff5d29d8e718cc390a4f5f48f2f2a3/src/machine.rs#L791), and can access the value the pointer is referring to. This is done by accessing the CPointerWrapper through `AllocExtra` as described above, in the `memory_read` hook function.

However, we are not sure how to modify the use of the MIRI memory access to actually use these values read from C. Since `memory_read` is a read-only function, we can’t use it to modify the state of the interpreter. This function is a hook provided by rustc, so modification of the function signature itself seems like an invasive (and generally bad) idea. 
Following is the start of the `memory_read` hook function, where the C pointer reads are intercepted.
```[rust]
fn memory_read(
    _tcx: TyCtxt<'tcx>,
    machine: &Self,
    alloc_extra: &AllocExtra,
    (alloc_id, tag): (AllocId, Self::TagExtra),
    range: AllocRange,
) -> InterpResult<'tcx> {
    if let Some(foreign_items) = &alloc_extra.foreign_items {
        let key = foreign_items.get_internal_C_ptr_key();
        let ptr_rep = machine.foreign_items.borrow().get_internal_C_pointer_wrapper(key).unwrap();
        // at this point we have the C pointer
        // we can read the C pointer and get the value back
        // but how to actually give this value to MIRI so it’ll be used in 
        // further computations?
    }
    // ...
```

Some potential solutions to this issue could be:
* including the evaluation context as a component of `AllocExtra`, perhaps in a `RefCell`, so it can be modified 
* finding another hook function to use for read interception?

We would be interested in maintainers’ opinions on how to hook this access.

### Synchronizing memory
In this section, we talk about the synchronization of memory between MIRI and C. This is separate from the memory allocation problem: since there can be shared values/memory locations between the Rust and C programs, this known shared memory must be synced before and after every external call.

If a pointer to shared C memory is used in the Rust running in Miri, then the value of this memory location may be modified. Thus, if there is another C call later, any modifications to this C pointer need to be synchronized back into the C memory so that C has access to these changes. The same reasoning applies to Miri pointers that are passed as arguments to C functions: we need to allocate memory and store the abstract Miri value in a way that C has access to.

This synchronization works as follows, in [our function `call_and_add_external_c_fct_to_context`](https://github.com/emarteca/miri/blob/4c44e920edff5d29d8e718cc390a4f5f48f2f2a3/src/helpers.rs#L820) for dispatching calls to external C functions.
1. Sync shared memory from MIRI to C
2. Call the C function
3. Sync shared memory from C to MIRI

The synchronization from MIRI to C and from C to MIRI is done in almost the same way: 
1. Iterate over all the memory locations tagged as `CInternal`
2. For each of these locations:
    a. read the value from the memory in language A
    b. convert the value to its corresponding form in language B
    c. write the value to the memory in language B (overwriting the value that was previously stored there)

Note that the synchronization is only for memory we know is shared: it is not the same mechanism as for allocating new MIRI memory for returned C pointers.
The following code is the function for synchronizing shared memory locations from C to MIRI, for values of type `*i32`.
```[rust]
// iterate over all of the memory locations that are marked CInternal 
// and sync them from their C locations
fn sync_C_to_miri(&mut self) -> InterpResult<'tcx, ()> {
    let this = self.eval_context_mut();

    let c_mems = this.machine.foreign_items.borrow().get_internal_C_pointer_wrappers();
    let miri_ptrs_to_c_ptrs = this.machine.foreign_items.borrow().get_MIRI_pointers_to_C_pointers();
    for (ptr_id, cptr) in c_mems {
        unsafe {
            match cptr {
                CPointerWrapper::Mutable(MutableCPointerWrapper::I32(rptr), buffer_size) => {
                    // read the value from the C pointer and store it in mem
                    let c_i32 = std::slice::from_raw_parts_mut(rptr, buffer_size); 
                    let ptr_as_u8_stream = c_i32.iter().flat_map(|val| val.to_ne_bytes()).collect::<Vec<u8>>();
                    let miri_ptr = miri_ptrs_to_c_ptrs.get_by_right(&ptr_id).unwrap();
                    // this is the same pointer as before, with the same buffer size
                    // so let's just overwrite the old pointer 
                    this.write_bytes_ptr((*miri_ptr).into(), ptr_as_u8_stream)?;
                },
                _ => {}
            }
        }
    }
    Ok(())
}
```

#### When should we synchronize memory?
For starters, we will synchronize all the shared memory at every cross-language call, but in the future we can add optimizations by only synchronizing what is required. One particular example is that if a pointer is const then we know the memory will not be modified and therefore there is no need to re-sync. There are probably contexts in which we can detect that synchronization is not required.

## Envisioned complexities
This section is a list of aspects we expect to be challenging.
* Anything that involves memory reallocation
    * reallocations in the MIRI memory (for example, if the memory we’ve allocated for a shared C pointer is insufficient)
    * reallocations in C or Rust that must be reflected in the other language upon syncing
* Flex arrays
* Keeping track of what memory should be contiguous, and making sure these relationships are maintained in the memory of both languages




