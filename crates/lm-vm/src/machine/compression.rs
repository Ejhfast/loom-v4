//! Compression instructions.

use super::*;

impl Machine {
    pub(super) fn exec_compression_native(&mut self, instr: Instr) -> Result<(), FaultCode> {
        use lm_bytecode::NativeInstr;

        let argument = self.pop_int()?;
        let format_value = self.pop_int()?;
        let input = self.pop_obj()?;
        let format = compression_format(format_value)?;
        match instr {
            Instr::Native(NativeInstr::CompressEncode) => {
                let level = u32::try_from(argument).map_err(|_| BAD_STATE)?;
                let output = {
                    let bytes = bytes_value(self, input)?;
                    lm_compress::compress(bytes, format, level).map_err(compress_fault)?
                };
                self.reserve(output.len(), &[Value::Obj(input)])?;
                let value = self.alloc(Object::Bytes(SharedBytes::from(output)))?;
                self.push(value)?;
            }
            Instr::Native(NativeInstr::CompressDecodeStatus) => {
                self.pending_decompression = None;
                let limit = match usize::try_from(argument) {
                    Ok(limit) => limit,
                    Err(_) => {
                        self.push(Value::Int(2))?;
                        return Ok(());
                    }
                };
                let result = {
                    let bytes = bytes_value(self, input)?;
                    lm_compress::decompress(bytes, format, limit)
                };
                let status = match result {
                    Ok(output) => {
                        self.reserve(output.len(), &[Value::Obj(input)])?;
                        let value = self.alloc(Object::Bytes(SharedBytes::from(output)))?;
                        let output = value.as_obj().ok_or(BAD_STATE)?;
                        self.pending_decompression = Some(PendingDecompression {
                            input,
                            format: format_value,
                            limit: argument,
                            output,
                        });
                        0
                    }
                    Err(lm_compress::DecompressError::InvalidData) => 1,
                    Err(lm_compress::DecompressError::Limit) => 2,
                    Err(lm_compress::DecompressError::Allocation) => {
                        return Err(FaultCode::HeapLimit);
                    }
                };
                self.push(Value::Int(status))?;
            }
            Instr::Native(NativeInstr::CompressDecodeValue) => {
                let cached = self.pending_decompression.take();
                let value = match cached {
                    Some(pending)
                        if pending.input == input
                            && pending.format == format_value
                            && pending.limit == argument
                            && self.vm.heap.try_get(pending.output).is_some() =>
                    {
                        Value::Obj(pending.output)
                    }
                    _ => {
                        let limit = usize::try_from(argument).map_err(|_| BAD_STATE)?;
                        let output = {
                            let bytes = bytes_value(self, input)?;
                            match lm_compress::decompress(bytes, format, limit) {
                                Ok(output) => output,
                                Err(lm_compress::DecompressError::Allocation) => {
                                    return Err(FaultCode::HeapLimit);
                                }
                                Err(_) => return Err(BAD_STATE),
                            }
                        };
                        self.reserve(output.len(), &[Value::Obj(input)])?;
                        self.alloc(Object::Bytes(SharedBytes::from(output)))?
                    }
                };
                self.push(value)?;
            }
            _ => unreachable!("the compression dispatcher receives one compression instruction"),
        }
        Ok(())
    }
}

pub(crate) fn compression_format(value: i64) -> Result<lm_compress::Format, FaultCode> {
    match value {
        0 => Ok(lm_compress::Format::Gzip),
        1 => Ok(lm_compress::Format::Zlib),
        _ => Err(BAD_STATE),
    }
}

fn bytes_value(machine: &Machine, reference: ObjRef) -> Result<&[u8], FaultCode> {
    match machine.vm.heap.try_get(reference) {
        Some(Object::Bytes(bytes)) => Ok(bytes.as_slice()),
        _ => Err(BAD_TYPE),
    }
}

fn compress_fault(error: lm_compress::CompressError) -> FaultCode {
    match error {
        lm_compress::CompressError::Allocation => FaultCode::HeapLimit,
        lm_compress::CompressError::InvalidLevel | lm_compress::CompressError::Backend => BAD_STATE,
    }
}
