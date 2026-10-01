use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MappingPoint {
    pub x: f64,
    pub y: f64,
}

pub fn validate_curve(curve: &[MappingPoint], y_min: f64, y_max: f64) -> Result<(), String> {
    if !(2..=6).contains(&curve.len()) {
        return Err("映射曲线必须包含 2..=6 个节点".to_owned());
    }
    if !y_min.is_finite() || !y_max.is_finite() || y_min > y_max {
        return Err("映射值范围无效".to_owned());
    }
    if curve.first().map(|point| point.x) != Some(0.0)
        || curve.last().map(|point| point.x) != Some(1.0)
    {
        return Err("映射曲线的首尾位置必须分别为 0 和 1".to_owned());
    }
    for (index, point) in curve.iter().enumerate() {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err("映射节点必须使用有限数值".to_owned());
        }
        if !(0.0..=1.0).contains(&point.x) || !(y_min..=y_max).contains(&point.y) {
            return Err(format!("第 {} 个映射节点超出允许范围", index + 1));
        }
        if index > 0 && point.x <= curve[index - 1].x {
            return Err("映射节点的位置必须严格递增".to_owned());
        }
    }
    Ok(())
}

/// 对已验证的曲线进行分段线性插值，坐标超出面板时采用最近端点。
pub fn map_curve(curve: &[MappingPoint], x: f64) -> f64 {
    let Some(first) = curve.first() else {
        return 0.0;
    };
    let x = if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    for points in curve.windows(2) {
        if x <= points[1].x {
            let span = points[1].x - points[0].x;
            if span <= 0.0 {
                return points[0].y;
            }
            let position = ((x - points[0].x) / span).clamp(0.0, 1.0);
            return points[0].y + position * (points[1].y - points[0].y);
        }
    }
    curve.last().unwrap_or(first).y
}

/// 当前模式使用的周期范围为 10..=100ms，该范围的设备编码与毫秒值相同。
pub fn period_ms_to_frequency(period_ms: f64) -> u8 {
    if !period_ms.is_finite() {
        return 10;
    }
    period_ms.round().clamp(10.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> [MappingPoint; 3] {
        [
            MappingPoint { x: 0.0, y: 0.0 },
            MappingPoint { x: 0.5, y: 100.0 },
            MappingPoint { x: 1.0, y: 0.0 },
        ]
    }

    #[test]
    fn mapping_interpolates_and_clamps_at_endpoints() {
        let curve = curve();
        assert_eq!(map_curve(&curve, 0.25), 50.0);
        assert_eq!(map_curve(&curve, 0.75), 50.0);
        assert_eq!(map_curve(&curve, -1.0), 0.0);
        assert_eq!(map_curve(&curve, 2.0), 0.0);
        assert_eq!(period_ms_to_frequency(22.6), 23);
        assert_eq!(period_ms_to_frequency(2.0), 10);
        assert_eq!(period_ms_to_frequency(120.0), 100);
    }

    #[test]
    fn mapping_rejects_invalid_nodes_before_interpolation() {
        let mut curve = curve();
        assert!(validate_curve(&curve, 0.0, 100.0).is_ok());
        curve[1].x = 0.0;
        assert!(validate_curve(&curve, 0.0, 100.0).is_err());
        curve[1].x = 0.5;
        curve[1].y = f64::NAN;
        assert!(validate_curve(&curve, 0.0, 100.0).is_err());
        curve[1].y = 101.0;
        assert!(validate_curve(&curve, 0.0, 100.0).is_err());
        curve[1].y = 50.0;
        curve[2].x = 0.9;
        assert!(validate_curve(&curve, 0.0, 100.0).is_err());
        assert!(validate_curve(&curve[..1], 0.0, 100.0).is_err());
    }
}
