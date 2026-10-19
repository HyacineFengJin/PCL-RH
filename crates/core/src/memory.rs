//! Heap recommendations are a bounded heuristic, not a compatibility check or
//! reservation of system RAM. Desktop callers collect a fresh memory snapshot
//! and enabled-mod count immediately before building the actual launch plan.
use serde::{Deserialize, Serialize};

pub const GIB: u64 = 1024 * 1024 * 1024;

/// Old settings remain manual when the new field is absent. Switching modes
/// does not overwrite the user's saved manual allocation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    #[default]
    Manual,
    Auto,
}

#[derive(Clone, Copy, Debug)]
pub struct MemorySnapshot {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recommendation {
    pub memory_gib: u32,
    pub constrained: bool,
}

pub fn recommend(snapshot: MemorySnapshot, mod_count: u32) -> Result<Recommendation, String> {
    if snapshot.total_bytes == 0 || snapshot.available_bytes > snapshot.total_bytes {
        return Err("系统内存信息无效，无法自动分配游戏内存".into());
    }
    // Leave 1–4 GiB of the currently available RAM for native allocations and
    // other applications. Half of physical RAM also caps large idle machines;
    // a very large Java heap is not automatically useful for a small instance.
    let reserve = (snapshot.total_bytes / 8).clamp(GIB, 4 * GIB);
    let budget =
        (snapshot.total_bytes / 2).min(snapshot.available_bytes.saturating_sub(reserve)) / GIB;
    if budget < 2 {
        return Err("当前可用内存不足以自动分配 2 GiB，请关闭其他程序或手动设置内存".into());
    }
    let desired = 2u32.saturating_add(mod_count.div_ceil(60)).clamp(3, 12);
    let memory_gib = u64::from(desired).min(budget) as u32;
    Ok(Recommendation {
        memory_gib,
        constrained: memory_gib < desired,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(total: u64, available: u64) -> MemorySnapshot {
        MemorySnapshot {
            total_bytes: total * GIB,
            available_bytes: available * GIB,
        }
    }
    #[test]
    fn old_mode_is_manual_and_invalid_modes_are_rejected() {
        assert_eq!(MemoryMode::default(), MemoryMode::Manual);
        assert_eq!(
            serde_json::to_string(&MemoryMode::Auto).unwrap(),
            "\"auto\""
        );
        assert!(serde_json::from_str::<MemoryMode>("\"unknown\"").is_err());
    }
    #[test]
    fn small_instances_do_not_fill_idle_large_machines() {
        assert_eq!(recommend(snapshot(64, 60), 0).unwrap().memory_gib, 3);
        assert_eq!(recommend(snapshot(64, 60), 60).unwrap().memory_gib, 3);
        assert_eq!(recommend(snapshot(64, 60), 240).unwrap().memory_gib, 6);
        assert_eq!(
            recommend(snapshot(64, 60), u32::MAX).unwrap().memory_gib,
            12
        );
    }
    #[test]
    fn available_memory_and_system_headroom_bound_modded_instances() {
        assert_eq!(
            recommend(snapshot(32, 8), 500).unwrap(),
            Recommendation {
                memory_gib: 4,
                constrained: true
            }
        );
        assert_eq!(
            recommend(snapshot(4, 3), 0).unwrap(),
            Recommendation {
                memory_gib: 2,
                constrained: true
            }
        );
        assert_eq!(recommend(snapshot(16, 15), 500).unwrap().memory_gib, 8);
    }
    #[test]
    fn insufficient_or_invalid_memory_never_silently_becomes_a_manual_value() {
        for snapshot in [
            snapshot(32, 5),
            snapshot(2, 2),
            snapshot(0, 0),
            snapshot(8, 9),
        ] {
            assert!(recommend(snapshot, 0).is_err());
        }
    }
    #[test]
    fn fractional_capacity_is_rounded_down_and_large_counts_do_not_overflow() {
        let result = recommend(
            MemorySnapshot {
                total_bytes: 32 * GIB,
                available_bytes: 8 * GIB - 1,
            },
            u32::MAX,
        )
        .unwrap();
        assert_eq!(result.memory_gib, 3);
        assert!(result.constrained);
        assert_eq!(
            recommend(
                MemorySnapshot {
                    total_bytes: u64::MAX,
                    available_bytes: u64::MAX
                },
                u32::MAX
            )
            .unwrap()
            .memory_gib,
            12
        );
    }
}
