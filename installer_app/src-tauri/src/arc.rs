use serde::Deserialize;
use std::path::Path;

const MAGIC: &[u8] = b"BC_ARC_END";
const FOOTER_LEN: usize = MAGIC.len() + 8 + 8;

#[derive(Deserialize)]
struct RawEntry {
    path: String,
    offset: u64,
    size: u64,
}

pub struct Arc {
    entries: Vec<RawEntry>,
    payload_start: usize,
    bytes: Vec<u8>,
    total_bytes: u64,
}

impl Arc {
    /// Read this executable and locate the appended manifest + payload footer.
    pub fn from_current_exe() -> Result<Arc, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&exe).map_err(|e| e.to_string())?;
        Self::from_bytes(bytes, &exe)
    }

    fn from_bytes(bytes: Vec<u8>, _exe: &Path) -> Result<Arc, String> {
        if bytes.len() < FOOTER_LEN {
            return Err("payload footer not found".into());
        }
        let footer = &bytes[bytes.len() - FOOTER_LEN..];
        if &footer[..MAGIC.len()] != MAGIC {
            return Err("not a BetterCopy installer payload".into());
        }
        let manifest_len = u64::from_le_bytes(
            footer[MAGIC.len()..MAGIC.len() + 8]
                .try_into()
                .map_err(|_| "bad manifest len")?,
        ) as usize;
        let payload_len = u64::from_le_bytes(
            footer[MAGIC.len() + 8..].try_into().map_err(|_| "bad payload len")?,
        ) as usize;

        let manifest_start = bytes.len() - FOOTER_LEN - manifest_len - payload_len;
        let manifest_bytes = &bytes[manifest_start..manifest_start + manifest_len];
        let entries: Vec<RawEntry> =
            serde_json::from_slice(manifest_bytes).map_err(|e| format!("bad manifest: {e}"))?;
        let payload_start = manifest_start + manifest_len;

        Ok(Arc {
            entries,
            payload_start,
            bytes,
            total_bytes: payload_len as u64,
        })
    }

    /// Extract every manifest entry under `base`, reporting progress.
    pub fn extract_to(
        &self,
        base: &Path,
        on_progress: impl Fn(u32, &str),
    ) -> Result<(), String> {
        let mut written: u64 = 0;
        for e in &self.entries {
            let rel = Path::new(&e.path);
            let dest = base.join(rel);
            let parent = dest.parent().ok_or("bad archive path")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;

            let start = self.payload_start + e.offset as usize;
            let end = start + e.size as usize;
            if end > self.bytes.len() {
                return Err(format!("corrupt entry: {}", e.path));
            }
            let data = &self.bytes[start..end];
            std::fs::write(&dest, data)
                .map_err(|ioe| format!("writing `{}`: {ioe}", e.path))?;
            written += e.size;

            let pct = if self.total_bytes == 0 {
                100
            } else {
                ((written * 100) / self.total_bytes).min(100) as u32
            };
            on_progress(pct, &format!("Extracting {}", e.path));
        }
        Ok(())
    }
}