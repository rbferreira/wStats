//! Sensors readable without a driver or admin rights:
//! - power from the CPU's RAPL energy counters, which Windows publishes as the
//!   "Energy Meter" performance object;
//! - GPU temperature/fan, gathered by the GPU collector (D3DKMT) and folded in here;
//! - battery charge/discharge power, from the battery collector.
//!
//! CPU die temperature, fans, and CPU power where Windows does not publish it (desktops,
//! AMD) need a driver: the sensor service reads those (see service.rs).

use super::model::{Reading, Sensors};
use super::pdh::{Counter, Query};

pub struct SensorCollector {
    power: Option<Counter>,
}

/// RAPL domain -> label. Unknown domains keep their raw name.
pub(super) fn domain_label(instance: &str, pt: bool) -> Option<&'static str> {
    let tr = |a, b| if pt { a } else { b };
    let domain = instance.rsplit('_').next()?;
    Some(match domain {
        "PKG" => tr("Pacote da CPU", "CPU package"),
        "PP0" => tr("Núcleos da CPU", "CPU cores"),
        "PP1" => tr("Gráficos integrados", "Integrated graphics"),
        "DRAM" => tr("Memória", "Memory"),
        "PSYS" => tr("Plataforma", "Platform"),
        _ => return None,
    })
}

/// Display order: package first, then its parts.
fn domain_rank(instance: &str) -> u8 {
    match instance.rsplit('_').next() {
        Some("PSYS") => 0,
        Some("PKG") => 1,
        Some("PP0") => 2,
        Some("PP1") => 3,
        Some("DRAM") => 4,
        _ => 5,
    }
}

impl SensorCollector {
    pub fn new(q: &Query) -> Self {
        Self { power: q.add(r"\Energy Meter(*)\Power") }
    }

    pub fn sample(&self, q: &Query) -> Sensors {
        let mut rows: Vec<(String, f64)> = q.array(self.power).into_iter().filter(|(n, _)| n != "_Total").collect();
        rows.sort_by_key(|(n, _)| domain_rank(n));
        Sensors {
            power: rows
                .into_iter()
                .map(|(name, mw)| Reading {
                    id: name.clone(),
                    label: domain_label(&name, crate::lang::pt()).map(str::to_string).unwrap_or(name),
                    value: (mw / 1000.0) as f32,
                })
                .collect(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapl_domains() {
        assert_eq!(domain_label("RAPL_Package0_PKG", true), Some("Pacote da CPU"));
        assert_eq!(domain_label("RAPL_Package0_PP1", true), Some("Gráficos integrados"));
        assert_eq!(domain_label("RAPL_Package0_PP1", false), Some("Integrated graphics"));
        assert_eq!(domain_label("Something", true), None);
        assert!(domain_rank("RAPL_Package0_PKG") < domain_rank("RAPL_Package0_DRAM"));
    }
}
