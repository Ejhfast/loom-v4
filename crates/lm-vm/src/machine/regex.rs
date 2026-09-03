//! Regular-expression instructions.

use super::*;
use lm_heap::{NativeRegexMatch, RegexCaptureRange};

impl Machine {
    pub(super) fn exec_regex_native(&mut self, instr: Instr) -> Result<(), FaultCode> {
        use lm_bytecode::NativeInstr;

        match instr {
            Instr::Native(NativeInstr::RegexCompileStatus) => {
                let pattern = self.pop_obj()?;
                self.pending_regex_compile = None;
                let result = lm_regex::Regex::compile(self.text_value(pattern)?.as_str());
                let status = match result {
                    Ok(regex) => {
                        let value = self.alloc(Object::NativeRegex(regex))?;
                        self.pending_regex_compile = value.as_obj();
                        0
                    }
                    Err(error) if error.kind() == lm_regex::CompileErrorKind::Limit => {
                        self.pending_regex_compile = None;
                        2
                    }
                    Err(_) => {
                        self.pending_regex_compile = None;
                        1
                    }
                };
                self.push(Value::Int(status))?;
            }
            Instr::Native(NativeInstr::RegexCompileValue) => {
                let pattern = self.pop_obj()?;
                let cached = self.pending_regex_compile.take();
                let source = self.text_value(pattern)?.as_str();
                let value = match cached {
                    Some(reference)
                        if matches!(
                            self.vm.heap.try_get(reference),
                            Some(Object::NativeRegex(regex)) if regex.source() == source
                        ) =>
                    {
                        Value::Obj(reference)
                    }
                    _ => {
                        let regex = lm_regex::Regex::compile(source).map_err(|_| BAD_STATE)?;
                        self.alloc(Object::NativeRegex(regex))?
                    }
                };
                self.push(value)?;
            }
            Instr::Native(NativeInstr::RegexSource) => {
                let reference = self.pop_obj()?;
                let source = self.regex_value(reference)?.source().to_string();
                let source =
                    SharedText::try_from_string(source).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(source))?;
                self.push(value)?;
            }
            Instr::Native(NativeInstr::RegexIsMatch) => {
                let text = self.pop_obj()?;
                let regex = self.pop_obj()?;
                let found = self
                    .regex_value(regex)?
                    .is_match(self.text_value(text)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(NativeInstr::RegexCount) => {
                let text = self.pop_obj()?;
                let regex = self.pop_obj()?;
                let count = self
                    .regex_value(regex)?
                    .count(self.text_value(text)?.as_str());
                let count = i64::try_from(count).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(count))?;
            }
            Instr::Native(NativeInstr::RegexSplit) => self.exec_regex_split()?,
            Instr::Native(NativeInstr::RegexReplaceAll) => self.exec_regex_replace_all()?,
            Instr::Native(NativeInstr::RegexMatchStart) => {
                let reference = self.pop_obj()?;
                let start = self.regex_match_value(reference)?.start;
                self.push(Value::Int(i64::from(start)))?;
            }
            Instr::Native(NativeInstr::RegexMatchEnd) => {
                let reference = self.pop_obj()?;
                let end = self.regex_match_value(reference)?.end;
                self.push(Value::Int(i64::from(end)))?;
            }
            Instr::Native(NativeInstr::RegexMatchText) => {
                let reference = self.pop_obj()?;
                let text = self.regex_match_value(reference)?.text.clone();
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(NativeInstr::RegexMatchGroupCount) => {
                let reference = self.pop_obj()?;
                let count = self.regex_match_value(reference)?.groups.len();
                let count = i64::try_from(count).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(count))?;
            }
            _ => unreachable!("the regex dispatcher receives one regex instruction"),
        }
        Ok(())
    }

    pub(super) fn exec_regex_captures(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
    ) -> Result<(), FaultCode> {
        let text_reference = self.pop_obj()?;
        let regex_reference = self.pop_obj()?;
        let family = self.close_option_family(module, envs, ty)?;
        let text = self.text_value(text_reference)?;
        let regex = self.regex_value(regex_reference)?;
        let Some(matched) = build_regex_match(regex, text.as_str())? else {
            self.push(Value::EmptyCase { ty: family, arm: 1 })?;
            return Ok(());
        };
        let value = self.alloc(Object::NativeRegexMatch(Box::new(matched)))?;
        self.push(value)
    }

    pub(super) fn exec_regex_match_group(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
    ) -> Result<(), FaultCode> {
        let index = self.pop_int()?;
        let reference = self.pop_obj()?;
        let family = self.close_option_family(module, envs, ty)?;
        let index = match usize::try_from(index) {
            Ok(index) => index,
            Err(_) => {
                self.push(Value::EmptyCase { ty: family, arm: 1 })?;
                return Ok(());
            }
        };
        let text = {
            let matched = self.regex_match_value(reference)?;
            match matched.groups.get(index).copied().flatten() {
                Some(range) => Some(regex_group_text(matched, range).ok_or(BAD_STATE)?),
                None => None,
            }
        };
        self.push_regex_group(text, family)
    }

    pub(super) fn exec_regex_match_named(
        &mut self,
        module: &NamespaceRuntime,
        envs: &mut TypeEnvs,
        ty: u32,
    ) -> Result<(), FaultCode> {
        let name = self.pop_obj()?;
        let reference = self.pop_obj()?;
        let family = self.close_option_family(module, envs, ty)?;
        let text = {
            let name = self.text_value(name)?.as_str();
            let matched = self.regex_match_value(reference)?;
            let range = matched
                .names
                .iter()
                .find_map(|(candidate, index)| (candidate == name).then_some(*index as usize))
                .and_then(|index| matched.groups.get(index).copied().flatten());
            match range {
                Some(range) => Some(regex_group_text(matched, range).ok_or(BAD_STATE)?),
                None => None,
            }
        };
        self.push_regex_group(text, family)
    }

    fn exec_regex_split(&mut self) -> Result<(), FaultCode> {
        let text_reference = self.pop_obj()?;
        let regex_reference = self.pop_obj()?;
        let text = self.text_value(text_reference)?;
        let regex = self.regex_value(regex_reference)?;
        let ranges = regex
            .split_range_iter(text.as_str())
            .map(|range| (range.start, range.end));
        let pieces = self
            .vm
            .heap
            .try_text_range_view_batch(text_reference, ranges)
            .ok_or(BAD_TYPE)?
            .map_err(|_| FaultCode::HeapLimit)?
            .ok_or(FaultCode::HeapLimit)?;
        let cost = Heap::text_view_list_base_cost(pieces.len()).ok_or(FaultCode::HeapLimit)?;
        self.reserve(
            cost,
            &[Value::Obj(regex_reference), Value::Obj(text_reference)],
        )?;
        let reference = self
            .vm
            .heap
            .try_alloc_text_view_list(pieces)
            .ok_or(FaultCode::HeapLimit)?;
        self.push(Value::Obj(reference))
    }

    fn exec_regex_replace_all(&mut self) -> Result<(), FaultCode> {
        let replacement = self.pop_obj()?;
        let text = self.pop_obj()?;
        let regex = self.pop_obj()?;
        let output = {
            let replacement = self.text_value(replacement)?;
            let text = self.text_value(text)?;
            let regex = self.regex_value(regex)?;
            let limit = self.vm.heap.stats().cap_bytes;
            regex
                .replace_all(text.as_str(), replacement.as_str(), limit)
                .map_err(|_| FaultCode::HeapLimit)?
        };
        let output = SharedText::try_from_string(output).map_err(|_| FaultCode::HeapLimit)?;
        let value = self.alloc(Object::Str(output))?;
        self.push(value)
    }

    fn regex_value(&self, reference: ObjRef) -> Result<&lm_regex::Regex, FaultCode> {
        match self.vm.heap.get(reference) {
            Object::NativeRegex(regex) => Ok(regex),
            _ => Err(BAD_TYPE),
        }
    }

    fn regex_match_value(&self, reference: ObjRef) -> Result<&NativeRegexMatch, FaultCode> {
        match self.vm.heap.get(reference) {
            Object::NativeRegexMatch(matched) => Ok(matched),
            _ => Err(BAD_TYPE),
        }
    }

    fn push_regex_group(
        &mut self,
        text: Option<SharedText>,
        family: ClosedTypeId,
    ) -> Result<(), FaultCode> {
        let Some(text) = text else {
            return self.push(Value::EmptyCase { ty: family, arm: 1 });
        };
        let value = self.alloc(Object::Substring(text))?;
        self.push(value)
    }
}

pub(crate) fn build_regex_match(
    regex: &lm_regex::Regex,
    text: &str,
) -> Result<Option<NativeRegexMatch>, FaultCode> {
    let Some(captures) = regex.captures(text) else {
        return Ok(None);
    };
    let complete = captures.complete();
    let matched = SharedText::try_from_str(&text[complete.start..complete.end])
        .map_err(|_| FaultCode::HeapLimit)?;
    let mut groups = Vec::new();
    groups
        .try_reserve_exact(captures.len())
        .map_err(|_| FaultCode::HeapLimit)?;
    for group in captures.groups() {
        let range = match group {
            Some(range) => Some(RegexCaptureRange {
                start: u32::try_from(range.start.checked_sub(complete.start).ok_or(BAD_STATE)?)
                    .map_err(|_| FaultCode::HeapLimit)?,
                end: u32::try_from(range.end.checked_sub(complete.start).ok_or(BAD_STATE)?)
                    .map_err(|_| FaultCode::HeapLimit)?,
            }),
            None => None,
        };
        groups.push(range);
    }
    let mut names = Vec::new();
    for (index, name) in regex.capture_names() {
        names.try_reserve(1).map_err(|_| FaultCode::HeapLimit)?;
        names.push((
            try_copy_text(name)?,
            u32::try_from(index).map_err(|_| BAD_STATE)?,
        ));
    }
    Ok(Some(NativeRegexMatch {
        text: matched,
        start: u32::try_from(complete.start).map_err(|_| FaultCode::HeapLimit)?,
        end: u32::try_from(complete.end).map_err(|_| FaultCode::HeapLimit)?,
        groups: groups.into_boxed_slice(),
        names: names.into_boxed_slice(),
    }))
}

pub(crate) fn regex_group_text(
    matched: &NativeRegexMatch,
    range: RegexCaptureRange,
) -> Option<SharedText> {
    matched.text.slice(range.start as usize, range.end as usize)
}

fn try_copy_text(source: &str) -> Result<String, FaultCode> {
    let mut text = String::new();
    text.try_reserve_exact(source.len())
        .map_err(|_| FaultCode::HeapLimit)?;
    text.push_str(source);
    Ok(text)
}
