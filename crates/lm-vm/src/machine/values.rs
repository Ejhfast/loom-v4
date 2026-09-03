//! Text, bytes, character, and builder instructions.

use super::*;

impl Machine {
    /// Execute one native value instruction outside the hot dispatch body.
    #[inline(never)]
    pub(super) fn exec_native_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::EqStr) => self.str_compare(true),
            Instr::Native(lm_bytecode::NativeInstr::NeStr) => self.str_compare(false),
            Instr::Native(
                lm_bytecode::NativeInstr::StrByteLen
                | lm_bytecode::NativeInstr::StrCharCount
                | lm_bytecode::NativeInstr::TextHash
                | lm_bytecode::NativeInstr::StrConcat
                | lm_bytecode::NativeInstr::StrStartsWith
                | lm_bytecode::NativeInstr::StrEndsWith
                | lm_bytecode::NativeInstr::StrContains
                | lm_bytecode::NativeInstr::StrFindIndex
                | lm_bytecode::NativeInstr::TextFindByteIndex
                | lm_bytecode::NativeInstr::TextAtByte
                | lm_bytecode::NativeInstr::TextAt
                | lm_bytecode::NativeInstr::TextSlice
                | lm_bytecode::NativeInstr::TextIsBoundary
                | lm_bytecode::NativeInstr::TextSliceBytes
                | lm_bytecode::NativeInstr::TextBytes
                | lm_bytecode::NativeInstr::TextLt
                | lm_bytecode::NativeInstr::TextLe
                | lm_bytecode::NativeInstr::TextGt
                | lm_bytecode::NativeInstr::TextGe
                | lm_bytecode::NativeInstr::TextTrim
                | lm_bytecode::NativeInstr::TextTrimStart
                | lm_bytecode::NativeInstr::TextTrimEnd
                | lm_bytecode::NativeInstr::TextToLowerAscii
                | lm_bytecode::NativeInstr::TextToUpperAscii
                | lm_bytecode::NativeInstr::TextReplace
                | lm_bytecode::NativeInstr::TextParseIntStatus
                | lm_bytecode::NativeInstr::TextParseIntValue
                | lm_bytecode::NativeInstr::TextPadStart
                | lm_bytecode::NativeInstr::TextPadEnd
                | lm_bytecode::NativeInstr::TextToString,
            ) => self.exec_string_instr(instr),
            Instr::Native(
                lm_bytecode::NativeInstr::CharCodepoint
                | lm_bytecode::NativeInstr::CharUtf8Len
                | lm_bytecode::NativeInstr::EqChar
                | lm_bytecode::NativeInstr::NeChar
                | lm_bytecode::NativeInstr::LtChar
                | lm_bytecode::NativeInstr::LeChar
                | lm_bytecode::NativeInstr::GtChar
                | lm_bytecode::NativeInstr::GeChar,
            ) => self.exec_char_instr(instr),
            Instr::Native(
                lm_bytecode::NativeInstr::HashCombine
                | lm_bytecode::NativeInstr::HashUnorderedCombine,
            ) => {
                let value = self.pop_int()? as u64;
                let seed = self.pop_int()? as u64;
                let value = Self::stable_hash_mix(value.wrapping_add(0x9e37_79b9_7f4a_7c15));
                let hash = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::HashCombine) => {
                        Self::stable_hash_mix(seed ^ value)
                    }
                    Instr::Native(lm_bytecode::NativeInstr::HashUnorderedCombine) => {
                        seed.wrapping_add(value)
                    }
                    _ => unreachable!(),
                };
                self.push(Value::Int(hash as i64))
            }
            Instr::Native(
                lm_bytecode::NativeInstr::RegexCompileStatus
                | lm_bytecode::NativeInstr::RegexCompileValue
                | lm_bytecode::NativeInstr::RegexSource
                | lm_bytecode::NativeInstr::RegexIsMatch
                | lm_bytecode::NativeInstr::RegexCount
                | lm_bytecode::NativeInstr::RegexSplit
                | lm_bytecode::NativeInstr::RegexReplaceAll
                | lm_bytecode::NativeInstr::RegexMatchStart
                | lm_bytecode::NativeInstr::RegexMatchEnd
                | lm_bytecode::NativeInstr::RegexMatchText
                | lm_bytecode::NativeInstr::RegexMatchGroupCount,
            ) => self.exec_regex_native(instr),
            Instr::Native(_) => self.exec_bytes_builder_instr(instr),
            _ => unreachable!("the native dispatcher receives one native instruction"),
        }
    }

    /// Execute one immutable String instruction outside the hot dispatch body.
    #[inline(never)]
    pub(super) fn exec_string_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::StrByteLen) => {
                let string = self.pop_obj()?;
                let len = self.text_value(string)?.len();
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrCharCount) => {
                let string = self.pop_obj()?;
                let count = self.text_value(string)?.char_count();
                let count = i64::try_from(count).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(count))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextHash) => {
                let text = self.pop_obj()?;
                let hash = self.text_value(text)?.semantic_hash() as i64;
                self.push(Value::Int(hash))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrConcat) => {
                let other = self.pop_obj()?;
                let string = self.pop_obj()?;
                let string_text = self.text_value(string)?.to_shared();
                let other_text = self.text_value(other)?.to_shared();
                let len = string_text
                    .len()
                    .checked_add(other_text.len())
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(len, &[Value::Obj(string), Value::Obj(other)])?;
                let text = string_text
                    .try_concat(&other_text)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrStartsWith) => {
                let prefix = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .starts_with(self.text_value(prefix)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrEndsWith) => {
                let suffix = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .ends_with(self.text_value(suffix)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrContains) => {
                let needle = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .contains(self.text_value(needle)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrFindIndex) => {
                let needle = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .find_scalar(self.text_value(needle)?);
                let index = match found {
                    Some(index) => i64::try_from(index).map_err(|_| FaultCode::IntegerOverflow)?,
                    None => -1,
                };
                self.push(Value::Int(index))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex) => {
                let needle = self.pop_obj()?;
                let text = self.pop_obj()?;
                let found = self.text_value(text)?.find_byte(self.text_value(needle)?);
                let index = match found {
                    Some(index) => i64::try_from(index).map_err(|_| FaultCode::IntegerOverflow)?,
                    None => -1,
                };
                self.push(Value::Int(index))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextAtByte) => {
                let index = self.pop_int()?;
                let text = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let value = self
                    .text_value(text)?
                    .scalar_at_byte(index)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                self.push(Value::Char(value))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextAt) => {
                let index = self.pop_int()?;
                let text = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let value = self
                    .text_value(text)?
                    .scalar_at(index)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                self.push(Value::Char(value))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextSlice) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let text = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let slice = self
                    .text_value(text)?
                    .scalar_slice(start, length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Substring(slice))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary) => {
                let index = self.pop_int()?;
                let text = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let boundary = self.text_value(text)?.is_char_boundary(index);
                self.push(Value::Bool(boundary))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let text = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let end = start
                    .checked_add(length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let slice = self
                    .text_value(text)?
                    .slice(start, end)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Substring(slice))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextBytes) => {
                let text = self.pop_obj()?;
                let bytes = self.text_value(text)?.bytes();
                let value = self.alloc(Object::Bytes(bytes))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextTrim
                | lm_bytecode::NativeInstr::TextTrimStart
                | lm_bytecode::NativeInstr::TextTrimEnd,
            ) => {
                let text = self.pop_obj()?;
                let value = self.text_value(text)?;
                let source = value.as_str();
                // Both bounds come from the trimmed views of the same
                // text, so each one sits on a scalar boundary.
                let start = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd) => 0,
                    _ => source.len() - source.trim_start().len(),
                };
                let end = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::TextTrimStart) => source.len(),
                    _ => source.trim_end().len(),
                };
                let end = end.max(start);
                let slice = value.slice(start, end).ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Substring(slice))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextToLowerAscii
                | lm_bytecode::NativeInstr::TextToUpperAscii,
            ) => {
                let text = self.pop_obj()?;
                let value = self.text_value(text)?;
                // ASCII case mapping keeps every byte width, so the
                // result has the byte length of the input.
                let len = value.len();
                let lower = matches!(
                    instr,
                    Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii)
                );
                let mapped = if lower {
                    value.as_str().to_ascii_lowercase()
                } else {
                    value.as_str().to_ascii_uppercase()
                };
                self.reserve(len, &[Value::Obj(text)])?;
                let mapped =
                    SharedText::try_from_string(mapped).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(mapped))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextReplace) => {
                let replacement = self.pop_obj()?;
                let needle = self.pop_obj()?;
                let text = self.pop_obj()?;
                let source = self.text_value(text)?.as_str();
                let needle_text = self.text_value(needle)?.as_str();
                let replacement_text = self.text_value(replacement)?.as_str();
                // Size the result before the allocation. An empty
                // needle matches at every scalar boundary, so the
                // count comes from the match walk and never from a
                // caller-supplied length.
                let matches = source.match_indices(needle_text).count();
                let removed = matches
                    .checked_mul(needle_text.len())
                    .ok_or(FaultCode::HeapLimit)?;
                let added = matches
                    .checked_mul(replacement_text.len())
                    .ok_or(FaultCode::HeapLimit)?;
                let len = source
                    .len()
                    .checked_sub(removed)
                    .and_then(|kept| kept.checked_add(added))
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(
                    len,
                    &[
                        Value::Obj(text),
                        Value::Obj(needle),
                        Value::Obj(replacement),
                    ],
                )?;
                let source = self.text_value(text)?.as_str();
                let needle_text = self.text_value(needle)?.as_str();
                let replacement_text = self.text_value(replacement)?.as_str();
                let joined = source.replace(needle_text, replacement_text);
                let joined =
                    SharedText::try_from_string(joined).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(joined))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextParseIntStatus
                | lm_bytecode::NativeInstr::TextParseIntValue,
            ) => {
                let radix = self.pop_int()?;
                let text = self.pop_obj()?;
                let status = matches!(
                    instr,
                    Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus)
                );
                // Both operands can come from data, so neither one
                // faults. A radix outside 2 to 36 reports status 3.
                let radix = u32::try_from(radix)
                    .ok()
                    .filter(|radix| (2..=36).contains(radix));
                let Some(radix) = radix else {
                    self.push(Value::Int(if status { 3 } else { 0 }))?;
                    return Ok(());
                };
                let parsed = i64::from_str_radix(self.text_value(text)?.as_str(), radix);
                let answer = match (status, parsed) {
                    (true, Ok(_)) => 0,
                    (true, Err(error)) => match error.kind() {
                        std::num::IntErrorKind::PosOverflow
                        | std::num::IntErrorKind::NegOverflow => 2,
                        _ => 1,
                    },
                    (false, Ok(value)) => value,
                    (false, Err(_)) => 0,
                };
                self.push(Value::Int(answer))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextPadStart | lm_bytecode::NativeInstr::TextPadEnd,
            ) => {
                let width = self.pop_int()?;
                let text = self.pop_obj()?;
                let source = self.text_value(text)?.to_shared();
                let scalar_len =
                    i64::try_from(source.char_count()).map_err(|_| FaultCode::IntegerOverflow)?;
                let padding = width.saturating_sub(scalar_len);
                if padding <= 0 && matches!(self.vm.heap.try_get(text), Some(Object::Str(_))) {
                    self.push(Value::Obj(text))?;
                    return Ok(());
                }
                let padding = usize::try_from(padding.max(0)).map_err(|_| FaultCode::HeapLimit)?;
                let length = source
                    .len()
                    .checked_add(padding)
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(length, &[Value::Obj(text)])?;
                let mut output = String::new();
                output
                    .try_reserve_exact(length)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let before = matches!(instr, Instr::Native(lm_bytecode::NativeInstr::TextPadStart));
                if before {
                    for _ in 0..padding {
                        output.push(' ');
                    }
                }
                output.push_str(source.as_str());
                if !before {
                    for _ in 0..padding {
                        output.push(' ');
                    }
                }
                let output =
                    SharedText::try_from_string(output).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(output))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextLt
                | lm_bytecode::NativeInstr::TextLe
                | lm_bytecode::NativeInstr::TextGt
                | lm_bytecode::NativeInstr::TextGe,
            ) => {
                let right = self.pop_obj()?;
                let left = self.pop_obj()?;
                let ordering = self
                    .text_value(left)?
                    .as_str()
                    .cmp(self.text_value(right)?.as_str());
                let result = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::TextLt) => ordering.is_lt(),
                    Instr::Native(lm_bytecode::NativeInstr::TextLe) => !ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::TextGt) => ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::TextGe) => !ordering.is_lt(),
                    _ => unreachable!(),
                };
                self.push(Value::Bool(result))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextToString) => {
                let source = self.pop_obj()?;
                if matches!(self.vm.heap.try_get(source), Some(Object::Str(_))) {
                    self.push(Value::Obj(source))?;
                    return Ok(());
                }
                let text = self.text_value(source)?.to_shared();
                if !text.has_bounded_retention() {
                    self.reserve(text.len(), &[Value::Obj(source)])?;
                }
                let text = text.try_bounded().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            _ => unreachable!("the String dispatcher receives one String instruction"),
        }
        Ok(())
    }

    /// Execute one immediate Char instruction.
    #[inline(never)]
    pub(super) fn exec_char_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::CharCodepoint) => {
                let value = self.pop_char()?;
                self.push(Value::Int(i64::from(u32::from(value))))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len) => {
                let value = self.pop_char()?;
                self.push(Value::Int(value.len_utf8() as i64))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::EqChar
                | lm_bytecode::NativeInstr::NeChar
                | lm_bytecode::NativeInstr::LtChar
                | lm_bytecode::NativeInstr::LeChar
                | lm_bytecode::NativeInstr::GtChar
                | lm_bytecode::NativeInstr::GeChar,
            ) => {
                let right = self.pop_char()?;
                let left = self.pop_char()?;
                let result = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::EqChar) => left == right,
                    Instr::Native(lm_bytecode::NativeInstr::NeChar) => left != right,
                    Instr::Native(lm_bytecode::NativeInstr::LtChar) => left < right,
                    Instr::Native(lm_bytecode::NativeInstr::LeChar) => left <= right,
                    Instr::Native(lm_bytecode::NativeInstr::GtChar) => left > right,
                    Instr::Native(lm_bytecode::NativeInstr::GeChar) => left >= right,
                    _ => unreachable!(),
                };
                self.push(Value::Bool(result))?;
            }
            _ => unreachable!("the Char dispatcher receives one Char instruction"),
        }
        Ok(())
    }

    /// Execute one Bytes or builder instruction outside the hot dispatch body.
    #[inline(never)]
    pub(super) fn exec_bytes_builder_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::SbNew) => {
                let value = self.alloc(Object::StrBuilder(NativeStringBuilder::new()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendStr) => {
                let string = self.pop_obj()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let text_len = self.text_value(string)?.len();
                let growth = match self.vm.heap.get(builder) {
                    Object::StrBuilder(builder) => builder.reserve_growth(text_len),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(builder), Value::Obj(string)])?;
                    match self.vm.heap.get_mut(builder) {
                        Object::StrBuilder(builder) => {
                            if !builder
                                .try_reserve(text_len)
                                .map_err(|_| FaultCode::HeapLimit)?
                            {
                                return Err(FaultCode::InvalidVmState);
                            }
                        }
                        _ => return Err(BAD_TYPE),
                    }
                }
                if !self.vm.heap.append_string(builder, string) {
                    return Err(FaultCode::InvalidVmState);
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(builder);
                }
                self.push(Value::Obj(builder))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendInt) => {
                let value = self.pop_int()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                self.sb_append_int(builder, value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendBool) => {
                let value = self.pop_bool()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let text = if value { "true" } else { "false" };
                self.sb_append(builder, text)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbBuild) => {
                let builder = self.pop_obj()?;
                let (len, scalar_count, ascii) = match self.vm.heap.get(builder) {
                    Object::StrBuilder(builder) => {
                        let len = builder.byte_len().ok_or(FaultCode::InvalidVmState)?;
                        let scalar_count = builder.scalar_len().ok_or(FaultCode::InvalidVmState)?;
                        let ascii = builder.is_ascii().ok_or(FaultCode::InvalidVmState)?;
                        (len, scalar_count, ascii)
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(len, &[Value::Obj(builder)])?;
                let source = match self.vm.heap.get(builder) {
                    Object::StrBuilder(builder) => {
                        builder.buffer().ok_or(FaultCode::InvalidVmState)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                let text = SharedText::try_from_str_parts(source, scalar_count, ascii)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbLen) => {
                let builder = self.pop_obj()?;
                let len = match self.vm.heap.get(builder) {
                    Object::StrBuilder(text) => {
                        text.scalar_len().ok_or(FaultCode::InvalidVmState)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbByteLen) => {
                let builder = self.pop_obj()?;
                let len = match self.vm.heap.get(builder) {
                    Object::StrBuilder(text) => text.byte_len().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => {
                let value = self.pop_char()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let len = value.len_utf8();
                let growth = match self.vm.heap.get(builder) {
                    Object::StrBuilder(target) => target.reserve_growth(len),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(builder)])?;
                }
                match self.vm.heap.get_mut(builder) {
                    Object::StrBuilder(target) => {
                        if growth != 0
                            && !target.try_reserve(len).map_err(|_| FaultCode::HeapLimit)?
                        {
                            return Err(FaultCode::InvalidVmState);
                        }
                        if !target.push(value) {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(builder);
                }
                self.push(Value::Obj(builder))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbFinish) => {
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let (text, scalar_count, ascii) = match self.vm.heap.get_mut(builder) {
                    Object::StrBuilder(builder) => {
                        builder.finish().ok_or(FaultCode::InvalidVmState)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.vm.heap.recharge_local(builder);
                let text = SharedText::try_from_string_parts(text, scalar_count, ascii)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbClear) => {
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let cleared = match self.vm.heap.get_mut(builder) {
                    Object::StrBuilder(text) => text.clear(),
                    _ => return Err(BAD_TYPE),
                };
                if !cleared {
                    return Err(FaultCode::InvalidVmState);
                }
                self.push(Value::Obj(builder))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbNew) => {
                let value = self.alloc(Object::ByteBuf(NativeByteBuffer::new()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbAppend) => {
                let value = self.pop_int()?;
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let byte = u8::try_from(value).map_err(|_| FaultCode::IntegerOverflow)?;
                let growth = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.reserve_growth(1),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(buffer)])?;
                }
                match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(bytes) => {
                        if growth != 0 && !bytes.try_reserve(1).map_err(|_| FaultCode::HeapLimit)? {
                            return Err(FaultCode::InvalidVmState);
                        }
                        if !bytes.push(byte) {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(buffer);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbLen) => {
                let buffer = self.pop_obj()?;
                let len = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.len().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbBuild) => {
                let buffer = self.pop_obj()?;
                let len = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.len().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(len, &[Value::Obj(buffer)])?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(len)
                    .map_err(|_| FaultCode::HeapLimit)?;
                match self.vm.heap.get(buffer) {
                    Object::ByteBuf(source) => {
                        bytes.extend_from_slice(source.buffer().ok_or(FaultCode::InvalidVmState)?)
                    }
                    _ => return Err(BAD_TYPE),
                }
                let value = self.alloc(Object::Bytes(bytes.into()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbExtend) => {
                let source = self.pop_obj()?;
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let bytes = match self.vm.heap.get(source) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let growth = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(target) => target.reserve_growth(bytes.len()),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(buffer), Value::Obj(source)])?;
                }
                match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(target) => {
                        if growth != 0
                            && !target
                                .try_reserve(bytes.len())
                                .map_err(|_| FaultCode::HeapLimit)?
                        {
                            return Err(FaultCode::InvalidVmState);
                        }
                        if !target.extend(&bytes) {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(buffer);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbReserve) => {
                let additional = self.pop_int()?;
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let additional =
                    usize::try_from(additional).map_err(|_| FaultCode::IntegerOverflow)?;
                let growth = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.reserve_growth(additional),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(buffer)])?;
                }
                match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(bytes) => {
                        if growth != 0
                            && !bytes
                                .try_reserve(additional)
                                .map_err(|_| FaultCode::HeapLimit)?
                        {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(buffer);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbClear) => {
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let cleared = match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(bytes) => bytes.clear(),
                    _ => return Err(BAD_TYPE),
                };
                if !cleared {
                    return Err(FaultCode::InvalidVmState);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbFinish) => {
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let bytes = match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(buffer) => buffer.finish().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                self.vm.heap.recharge_local(buffer);
                let value = self.alloc(Object::Bytes(SharedBytes::from(bytes)))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbAt) => {
                let index = self.pop_int()?;
                let buffer = self.pop_obj()?;
                let bytes = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) if bytes.buffer().is_some() => bytes,
                    Object::ByteBuf(_) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_TYPE),
                };
                let value = usize::try_from(index)
                    .ok()
                    .and_then(|index| bytes.at(index))
                    .map(i64::from)
                    .unwrap_or(-1);
                self.push(Value::Int(value))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbFindFrom) => {
                let start = self.pop_int()?;
                let needle = self.pop_obj()?;
                let buffer = self.pop_obj()?;
                let needle = match self.vm.heap.get(needle) {
                    Object::Bytes(bytes) => bytes,
                    _ => return Err(BAD_TYPE),
                };
                let bytes = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) if bytes.buffer().is_some() => bytes,
                    Object::ByteBuf(_) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_TYPE),
                };
                let found = usize::try_from(start)
                    .ok()
                    .and_then(|start| bytes.find_from(needle.as_slice(), start))
                    .and_then(|index| i64::try_from(index).ok())
                    .unwrap_or(-1);
                self.push(Value::Int(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesNew) => {
                let string = self.pop_obj()?;
                let text = match self.vm.heap.get(string) {
                    Object::Str(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Bytes(text.bytes()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesLen) => {
                let bytes = self.pop_obj()?;
                let len = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.len(),
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesText) => {
                let bytes_ref = self.pop_obj()?;
                let bytes = match self.vm.heap.get(bytes_ref) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let view = bytes.utf8_view().ok_or(FaultCode::BadCast)?;
                if !view.has_bounded_retention() {
                    self.reserve(view.len(), &[Value::Obj(bytes_ref)])?;
                }
                let text = view.try_bounded().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesTextRange) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let bytes_ref = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let end = start
                    .checked_add(length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let bytes = match self.vm.heap.get(bytes_ref) {
                    Object::Bytes(bytes) => {
                        bytes.slice(start, end).ok_or(FaultCode::IndexOutOfBounds)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                let view = bytes.utf8_view().ok_or(FaultCode::BadCast)?;
                if !view.has_bounded_retention() {
                    self.reserve(view.len(), &[Value::Obj(bytes_ref)])?;
                }
                let text = view.try_bounded().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesAt) => {
                let index = self.pop_int()?;
                let bytes = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let byte = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.as_slice().get(index).copied(),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                self.push(Value::Int(i64::from(byte)))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesGet) => {
                let index = self.pop_int()?;
                let bytes = self.pop_obj()?;
                let byte =
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| match self.vm.heap.get(bytes) {
                            Object::Bytes(bytes) => bytes.as_slice().get(index).copied(),
                            _ => None,
                        });
                if !matches!(self.vm.heap.get(bytes), Object::Bytes(_)) {
                    return Err(BAD_TYPE);
                }
                self.push(Value::Int(byte.map(i64::from).unwrap_or(-1)))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesSlice) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let bytes = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let end = start
                    .checked_add(length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let slice = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.slice(start, end),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Bytes(slice))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesConcat) => {
                let other = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let (left, right) = match (self.vm.heap.get(bytes), self.vm.heap.get(other)) {
                    (Object::Bytes(left), Object::Bytes(right)) => (left.clone(), right.clone()),
                    _ => return Err(BAD_TYPE),
                };
                let len = left
                    .len()
                    .checked_add(right.len())
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(len, &[Value::Obj(bytes), Value::Obj(other)])?;
                let joined = left.try_concat(&right).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Bytes(joined))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith) => {
                let prefix = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(prefix)) {
                    (Object::Bytes(bytes), Object::Bytes(prefix)) => {
                        bytes.as_slice().starts_with(prefix.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextSplit | lm_bytecode::NativeInstr::TextLines,
            ) => {
                let split = matches!(instr, Instr::Native(lm_bytecode::NativeInstr::TextSplit));
                let separator = if split { Some(self.pop_obj()?) } else { None };
                let text = self.pop_obj()?;
                let pieces = match separator {
                    Some(reference) => {
                        let separator = self.text_value(reference)?;
                        self.vm
                            .heap
                            .try_split_text_view_batch(text, separator.as_str())
                            .ok_or(BAD_TYPE)?
                    }
                    None => self
                        .vm
                        .heap
                        .try_line_text_view_batch(text)
                        .ok_or(BAD_TYPE)?,
                }
                .map_err(|_| FaultCode::HeapLimit)?;
                // One Substring object and one list slot per piece.
                let cost =
                    Heap::text_view_list_base_cost(pieces.len()).ok_or(FaultCode::HeapLimit)?;
                let mut roots = vec![Value::Obj(text)];
                if let Some(reference) = separator {
                    roots.push(Value::Obj(reference));
                }
                self.reserve(cost, &roots)?;
                let reference = self
                    .vm
                    .heap
                    .try_alloc_text_view_list(pieces)
                    .ok_or(FaultCode::HeapLimit)?;
                self.push(Value::Obj(reference))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith) => {
                let suffix = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(suffix)) {
                    (Object::Bytes(bytes), Object::Bytes(suffix)) => {
                        bytes.as_slice().ends_with(suffix.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesContains) => {
                let needle = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(needle)) {
                    (Object::Bytes(bytes), Object::Bytes(needle)) => {
                        let needle = needle.as_slice();
                        needle.is_empty()
                            || bytes
                                .as_slice()
                                .windows(needle.len())
                                .any(|window| window == needle)
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex) => {
                let needle = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(needle)) {
                    (Object::Bytes(bytes), Object::Bytes(needle)) => {
                        let needle = needle.as_slice();
                        if needle.is_empty() {
                            Some(0)
                        } else {
                            bytes
                                .as_slice()
                                .windows(needle.len())
                                .position(|window| window == needle)
                        }
                    }
                    _ => return Err(BAD_TYPE),
                };
                let index = match found {
                    Some(index) => i64::try_from(index).map_err(|_| FaultCode::IntegerOverflow)?,
                    None => -1,
                };
                self.push(Value::Int(index))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesHex) => {
                let bytes_ref = self.pop_obj()?;
                let bytes = match self.vm.heap.get(bytes_ref) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let len = bytes.len().checked_mul(2).ok_or(FaultCode::HeapLimit)?;
                self.reserve(len, &[Value::Obj(bytes_ref)])?;
                let mut text = String::new();
                text.try_reserve_exact(len)
                    .map_err(|_| FaultCode::HeapLimit)?;
                const HEX: &[u8; 16] = b"0123456789abcdef";
                for byte in bytes.as_slice() {
                    text.push(char::from(HEX[(byte >> 4) as usize]));
                    text.push(char::from(HEX[(byte & 0x0f) as usize]));
                }
                let value = self.alloc(Object::Str(text.into()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8) => {
                let bytes = self.pop_obj()?;
                let valid = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.is_utf8(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(valid))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesHash) => {
                let bytes = self.pop_obj()?;
                let hash = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.semantic_hash() as i64,
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(hash))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::EqBytes)
            | Instr::Native(lm_bytecode::NativeInstr::NeBytes) => {
                let right = self.pop_obj()?;
                let left = self.pop_obj()?;
                let equal = match (self.vm.heap.get(left), self.vm.heap.get(right)) {
                    (Object::Bytes(left), Object::Bytes(right)) => left == right,
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(
                    equal == matches!(instr, Instr::Native(lm_bytecode::NativeInstr::EqBytes)),
                ))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::LtBytes
                | lm_bytecode::NativeInstr::LeBytes
                | lm_bytecode::NativeInstr::GtBytes
                | lm_bytecode::NativeInstr::GeBytes,
            ) => {
                let right = self.pop_obj()?;
                let left = self.pop_obj()?;
                let ordering = match (self.vm.heap.get(left), self.vm.heap.get(right)) {
                    (Object::Bytes(left), Object::Bytes(right)) => {
                        left.as_slice().cmp(right.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                let result = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::LtBytes) => ordering.is_lt(),
                    Instr::Native(lm_bytecode::NativeInstr::LeBytes) => !ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::GtBytes) => ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::GeBytes) => !ordering.is_lt(),
                    _ => unreachable!(),
                };
                self.push(Value::Bool(result))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesCompact) => {
                let reference = self.pop_obj()?;
                let bytes = match self.vm.heap.get(reference) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(bytes.len(), &[Value::Obj(reference)])?;
                let compact = bytes.try_compact().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Bytes(compact))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesTextView) => {
                let reference = self.pop_obj()?;
                let text = match self.vm.heap.get(reference) {
                    Object::Bytes(bytes) => bytes.utf8_view().ok_or(FaultCode::BadCast)?,
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Substring(text))?;
                self.push(value)?;
            }
            _ => unreachable!("the Bytes dispatcher receives one native value instruction"),
        }
        Ok(())
    }

    /// Append text to a string builder with a growth reservation.
    pub(super) fn sb_append(&mut self, sb: ObjRef, text: &str) -> Result<(), FaultCode> {
        let growth = match self.vm.heap.get(sb) {
            Object::StrBuilder(buf) => buf.reserve_growth(text.len()),
            _ => return Err(BAD_TYPE),
        }
        .ok_or(FaultCode::InvalidVmState)?;
        if growth != 0 {
            self.reserve(growth, &[Value::Obj(sb)])?;
        }
        match self.vm.heap.get_mut(sb) {
            Object::StrBuilder(buf) => {
                if growth != 0
                    && !buf
                        .try_reserve(text.len())
                        .map_err(|_| FaultCode::HeapLimit)?
                {
                    return Err(FaultCode::InvalidVmState);
                }
                if !buf.append_str(text) {
                    return Err(FaultCode::InvalidVmState);
                }
            }
            _ => return Err(BAD_TYPE),
        }
        if growth != 0 {
            self.vm.heap.recharge_local(sb);
        }
        self.push(Value::Obj(sb))
    }

    /// Append one integer without a temporary string allocation.
    pub(super) fn sb_append_int(&mut self, sb: ObjRef, value: i64) -> Result<(), FaultCode> {
        let length = integer_text_len(value);
        let growth = match self.vm.heap.get(sb) {
            Object::StrBuilder(buf) => buf.reserve_growth(length),
            _ => return Err(BAD_TYPE),
        }
        .ok_or(FaultCode::InvalidVmState)?;
        if growth != 0 {
            self.reserve(growth, &[Value::Obj(sb)])?;
        }
        match self.vm.heap.get_mut(sb) {
            Object::StrBuilder(buf) => {
                if growth != 0 && !buf.try_reserve(length).map_err(|_| FaultCode::HeapLimit)? {
                    return Err(FaultCode::InvalidVmState);
                }
                if !buf.append_int(value) {
                    return Err(FaultCode::InvalidVmState);
                }
            }
            _ => return Err(BAD_TYPE),
        }
        if growth != 0 {
            self.vm.heap.recharge_local(sb);
        }
        self.push(Value::Obj(sb))
    }
}
