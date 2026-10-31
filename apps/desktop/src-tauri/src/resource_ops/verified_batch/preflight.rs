//! Bounded metadata admission shared by publishers and download allocation.
//!
//! A permit only admits an allocator call after the complete batch passes its
//! role's limits. It is not generated-source ownership: publishers still check
//! the actual retained descriptor, owner, hash and archive before mutation.

type Result<T> = std::result::Result<T, String>;

pub(crate) struct SourceSpec<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub size: u64,
    pub sha512: &'a str,
}

pub(crate) struct Rules {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_batch_bytes: u64,
    pub count_error: &'static str,
    pub file_error: &'static str,
    pub overflow_error: &'static str,
    pub total_error: &'static str,
}

pub(crate) struct PreparedSources<'a> {
    sources: Vec<SourceSpec<'a>>,
}

/// Deliberately neither Clone nor serializable. The allocator consumes this
/// admission once; the returned source must retain its own real authority.
pub(crate) struct AllocationPermit<'a> {
    index: usize,
    source: SourceSpec<'a>,
}

pub(crate) fn prepare<'a>(
    sources: impl IntoIterator<Item = SourceSpec<'a>>,
    rules: Rules,
    mut validate: impl FnMut(&SourceSpec<'a>) -> Result<()>,
) -> Result<PreparedSources<'a>> {
    // Count rejection precedes row validation, matching both publisher entry
    // points. Never collect an unbounded producer iterator into memory.
    let sources: Vec<_> = sources
        .into_iter()
        .take(rules.max_files.saturating_add(1))
        .collect();
    if sources.len() > rules.max_files {
        return Err(rules.count_error.into());
    }
    let mut total = 0u64;
    for source in &sources {
        validate(source)?;
        if source.size == 0 || source.size > rules.max_file_bytes {
            return Err(rules.file_error.into());
        }
        total = total.checked_add(source.size).ok_or(rules.overflow_error)?;
        if total > rules.max_batch_bytes {
            return Err(rules.total_error.into());
        }
    }
    Ok(PreparedSources { sources })
}

impl<'a> PreparedSources<'a> {
    pub(crate) fn into_permits(self) -> impl Iterator<Item = AllocationPermit<'a>> {
        self.sources
            .into_iter()
            .enumerate()
            .map(|(index, source)| AllocationPermit { index, source })
    }
}

impl<'a> AllocationPermit<'a> {
    /// Consumes every non-Clone admission before one real batch allocator call.
    pub(crate) fn allocate_batch_with<T>(
        permits: Vec<Self>,
        allocate: impl FnOnce(Vec<(usize, SourceSpec<'a>)>) -> Result<T>,
    ) -> Result<T> {
        allocate(
            permits
                .into_iter()
                .map(|permit| (permit.index, permit.source))
                .collect(),
        )
    }

    pub(crate) fn index(&self) -> usize {
        self.index
    }

    #[cfg(test)]
    pub(crate) fn allocate_with<T>(
        self,
        allocate: impl FnOnce(SourceSpec<'a>) -> Result<T>,
    ) -> Result<T> {
        allocate(self.source)
    }
}
