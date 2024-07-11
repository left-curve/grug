use {
    crate::{Environment, Region, VmError, VmResult},
    data_encoding::BASE64,
    wasmer::{
        AsStoreMut, AsStoreRef, MemoryError, MemoryType, MemoryView, Pages, Tunables, WasmPtr,
    },
};

pub fn read_from_memory(
    env: &mut Environment,
    store: &impl AsStoreRef,
    region_ptr: u32,
) -> VmResult<Vec<u8>> {
    let memory = env.get_wasmer_memory(&store)?;

    // read region
    let region = read_region(&memory, region_ptr)?;

    // read memory area indicated by region
    let mut buf = vec![0u8; region.length as usize];
    memory.read(region.offset as u64, &mut buf)?;

    Ok(buf)
}

pub fn read_then_wipe(
    env: &mut Environment,
    store: &mut impl AsStoreMut,
    region_ptr: u32,
) -> VmResult<Vec<u8>> {
    let data = read_from_memory(env, store, region_ptr)?;
    env.call_function0(store, "deallocate", &[region_ptr.into()])?;

    Ok(data)
}

pub fn write_to_memory(
    env: &mut Environment,
    store: &mut impl AsStoreMut,
    data: &[u8],
) -> VmResult<u32> {
    // call the `allocate` export to reserve an area in Wasm memory
    let region_ptr: u32 = env
        .call_function1(store, "allocate", &[(data.len() as u32).into()])?
        .try_into()
        .map_err(VmError::ReturnType)?;
    let memory = env.get_wasmer_memory(&store)?;
    let mut region = read_region(&memory, region_ptr)?;
    // don't forget to update region length
    region.length = data.len() as u32;

    if region.length > region.capacity {
        return Err(VmError::RegionTooSmall {
            offset: region.offset,
            capacity: region.capacity,
            data: BASE64.encode(data),
        });
    }

    // write the data to the reserved area
    memory.write(region.offset as u64, data)?;

    // write the Region
    write_region(&memory, region_ptr, region)?;

    Ok(region_ptr)
}

fn read_region(memory: &MemoryView, offset: u32) -> VmResult<Region> {
    let wptr = <WasmPtr<Region>>::new(offset);
    wptr.deref(memory).read().map_err(Into::into)
    // TODO: do some sanity checks on the Region?
}

fn write_region(memory: &MemoryView, offset: u32, region: Region) -> VmResult<()> {
    let wptr = <WasmPtr<Region>>::new(offset);
    wptr.deref(memory).write(region).map_err(Into::into)
}

pub struct MemoryLimit<T> {
    /// Host and client memory is limited to this number of pages separately.
    /// So total limit is 2 * shared_limit.
    shared_limit: Pages,
    base: T,
}

impl<T: Tunables> MemoryLimit<T> {
    pub fn new(bytes: usize, base: T) -> Self {
        Self {
            shared_limit: u32::try_from(bytes / wasmer::WASM_MAX_PAGES as usize)
                .unwrap()
                .into(),
            base,
        }
    }

    fn ensure_memory(&self, ty: &MemoryType) -> Result<MemoryType, MemoryError> {
        if ty.minimum > self.shared_limit {
            Err(MemoryError::MinimumMemoryTooLarge {
                min_requested: ty.minimum,
                max_allowed: self.shared_limit,
            })
        } else {
            match ty.maximum {
                Some(max) if max > self.shared_limit => Err(MemoryError::MaximumMemoryTooLarge {
                    max_requested: max,
                    max_allowed: self.shared_limit,
                }),
                None => Ok(self.limit_max(ty)),
                _ => Ok(*ty),
            }
        }
    }

    fn limit_max(&self, ty: &MemoryType) -> MemoryType {
        let mut ty = *ty;
        ty.maximum = Some(ty.maximum.unwrap_or(self.shared_limit));
        ty
    }
}

impl<T: Tunables> Tunables for MemoryLimit<T> {
    fn memory_style(&self, ty: &MemoryType) -> wasmer::vm::MemoryStyle {
        let ty = &self.limit_max(ty);
        self.base.memory_style(ty)
    }

    fn create_host_memory(
        &self,
        ty: &MemoryType,
        style: &wasmer::vm::MemoryStyle,
    ) -> Result<wasmer::vm::VMMemory, wasmer::MemoryError> {
        let ty = &self.ensure_memory(ty)?;
        self.base.create_host_memory(ty, style)
    }

    unsafe fn create_vm_memory(
        &self,
        ty: &MemoryType,
        style: &wasmer::vm::MemoryStyle,
        vm_definition_location: std::ptr::NonNull<wasmer::vm::VMMemoryDefinition>,
    ) -> Result<wasmer::vm::VMMemory, wasmer::MemoryError> {
        let ty = &self.ensure_memory(ty)?;
        self.base
            .create_vm_memory(ty, style, vm_definition_location)
    }

    fn table_style(&self, table: &wasmer::TableType) -> wasmer::vm::TableStyle {
        self.base.table_style(table)
    }

    fn create_host_table(
        &self,
        ty: &wasmer::TableType,
        style: &wasmer::vm::TableStyle,
    ) -> Result<wasmer::vm::VMTable, String> {
        self.base.create_host_table(ty, style)
    }

    unsafe fn create_vm_table(
        &self,
        ty: &wasmer::TableType,
        style: &wasmer::vm::TableStyle,
        vm_definition_location: std::ptr::NonNull<wasmer::vm::VMTableDefinition>,
    ) -> Result<wasmer::vm::VMTable, String> {
        self.base.create_vm_table(ty, style, vm_definition_location)
    }
}
