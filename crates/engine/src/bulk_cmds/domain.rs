//! Common edit domains; labels never establish semantic compatibility.
use nodecraft_types::{Contract, ParamControl, ParamSpec};

pub fn shared_specs(contracts: &[&Contract]) -> Vec<ParamSpec> {
    let Some(first) = contracts.first() else { return Vec::new() };
    first.schema.params.iter().filter_map(|spec| common_spec(&spec.name, contracts)).collect()
}
/// Retain each parameter's true semantic owner across model-backed node instances.
pub fn shared_node_specs(contracts: &[&nodecraft_nodes::NodeContract]) -> Vec<ParamSpec> {
    let Some(first) = contracts.first() else { return Vec::new() };
    first
        .parameter_groups()
        .flat_map(|group| &group.schema.params)
        .filter_map(|spec| {
            let owners = contracts.iter().map(|contract| parameter_owner(contract, &spec.name)).collect::<Option<Vec<_>>>()?;
            common_spec(&spec.name, &owners)
        })
        .collect()
}
pub fn parameter_owner<'a>(contract: &'a nodecraft_nodes::NodeContract, name: &str) -> Option<&'a Contract> {
    contract.parameter_groups().find(|group| group.schema.params.iter().any(|spec| spec.name == name))
}
fn common_spec(name: &str, contracts: &[&Contract]) -> Option<ParamSpec> {
    let first = contracts.first()?;
    let mut spec = first.schema.params.iter().find(|spec| spec.name == name)?.clone();
    for contract in contracts.iter().skip(1) {
        first.compatible(name, contract).ok()?;
        let other = contract.schema.params.iter().find(|spec| spec.name == name)?;
        spec.control = intersect(&spec.control, &other.control)?;
        if spec.default != other.default {
            spec.default = None;
        }
    }
    if spec.default.as_ref().is_some_and(|value| spec.control.validate_value(value).is_err()) {
        spec.default = None;
    }
    spec.visible_if = None;
    Some(spec)
}
pub(super) fn intersect(a: &ParamControl, b: &ParamControl) -> Option<ParamControl> {
    use ParamControl::*;
    Some(match (a, b) {
        (Text { multiline: a, max_bytes: am }, Text { multiline: b, max_bytes: bm }) => Text { multiline: *a && *b, max_bytes: (*am).min(*bm) },
        (Choice { options: a }, Choice { options: b }) => {
            let options: Vec<_> = a.iter().filter(|item| b.iter().any(|other| item.value == other.value)).cloned().collect();
            if options.is_empty() {
                return None;
            }
            Choice { options }
        }
        (Slider { min: a, max: az, step: as_ }, Slider { min: b, max: bz, step: bs }) => {
            let min = a.iter().chain(b.iter()).copied().max_by(|a, b| a.get().total_cmp(&b.get()));
            let max = az.iter().chain(bz.iter()).copied().min_by(|a, b| a.get().total_cmp(&b.get()));
            if min.zip(max).is_some_and(|(a, b)| a.get() > b.get()) {
                return None;
            }
            Slider { min, max, step: if as_.get() > bs.get() { *as_ } else { *bs } }
        }
        (Integer { min: a, max: az, step: as_ }, Integer { min: b, max: bz, step: bs }) => {
            let offset = i128::from(i64::MIN);
            let coord = |v: i64| u128::try_from(i128::from(v) - offset).ok();
            let low = coord(a.unwrap_or(nodecraft_types::Integer(i64::MIN)).0.max(b.unwrap_or(nodecraft_types::Integer(i64::MIN)).0))?;
            let high = coord(az.unwrap_or(nodecraft_types::Integer(i64::MAX)).0.min(bz.unwrap_or(nodecraft_types::Integer(i64::MAX)).0))?;
            let (min, max, step) =
                progression(low, high, coord(a.unwrap_or(nodecraft_types::Integer(0)).0)?, as_.0, coord(b.unwrap_or(nodecraft_types::Integer(0)).0)?, bs.0)?;
            let signed = |v: u128| i64::try_from(i128::try_from(v).ok()?.checked_add(offset)?).ok().map(nodecraft_types::Integer);
            Integer { min: Some(signed(min)?), max: Some(signed(max)?), step: nodecraft_types::Unsigned(step) }
        }
        (Unsigned { min: a, max: az, step: as_ }, Unsigned { min: b, max: bz, step: bs }) => {
            let low = u128::from(a.unwrap_or(nodecraft_types::Unsigned(0)).0.max(b.unwrap_or(nodecraft_types::Unsigned(0)).0));
            let high = u128::from(az.unwrap_or(nodecraft_types::Unsigned(u64::MAX)).0.min(bz.unwrap_or(nodecraft_types::Unsigned(u64::MAX)).0));
            let (min, max, step) = progression(
                low,
                high,
                u128::from(a.unwrap_or(nodecraft_types::Unsigned(0)).0),
                as_.0,
                u128::from(b.unwrap_or(nodecraft_types::Unsigned(0)).0),
                bs.0,
            )?;
            Unsigned {
                min: Some(nodecraft_types::Unsigned(u64::try_from(min).ok()?)),
                max: Some(nodecraft_types::Unsigned(u64::try_from(max).ok()?)),
                step: nodecraft_types::Unsigned(step),
            }
        }
        (Seed { max: a }, Seed { max: b }) => Seed { max: a.iter().chain(b.iter()).copied().min() },
        (a, b) if a == b => a.clone(),
        _ => return None,
    })
}
// Generalized CRT in a nonnegative u64-sized coordinate domain. u128 products preserve
// full-width integer precision; no float conversions, scanning or unbounded iteration.
fn progression(low: u128, high: u128, a: u128, step_a: u64, b: u128, step_b: u64) -> Option<(u128, u128, u64)> {
    if low > high || step_a == 0 || step_b == 0 {
        return None;
    }
    let (mut old_r, mut r) = (i128::from(step_a), i128::from(step_b));
    let (mut old_s, mut s) = (1_i128, 0_i128);
    while r != 0 {
        let q = old_r / r;
        (old_r, r) = (r, old_r.checked_sub(q.checked_mul(r)?)?);
        (old_s, s) = (s, old_s.checked_sub(q.checked_mul(s)?)?);
    }
    let gcd = u128::try_from(old_r).ok()?;
    let (a, b) = (a % u128::from(step_a), b % u128::from(step_b));
    let diff = i128::try_from(b).ok()?.checked_sub(i128::try_from(a).ok()?)?;
    if diff % old_r != 0 {
        return None;
    }
    let modulus = u128::from(step_b) / gcd;
    let modulus_i = i128::try_from(modulus).ok()?;
    let k = if modulus == 1 {
        0
    } else {
        let factor = u128::try_from((diff / old_r).rem_euclid(modulus_i)).ok()?;
        let inverse = u128::try_from(old_s.rem_euclid(modulus_i)).ok()?;
        factor.checked_mul(inverse)? % modulus
    };
    let period = u128::from(step_a).checked_mul(modulus)?;
    let base = a.checked_add(u128::from(step_a).checked_mul(k)?)? % period;
    let first = if base < low { base.checked_add((low - base).div_ceil(period).checked_mul(period)?)? } else { base };
    if first > high {
        return None;
    }
    let last = first.checked_add(((high - first) / period).checked_mul(period)?)?;
    let step = if first == last { 1 } else { u64::try_from(period).ok()? };
    Some((first, last, step))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{Choice, Integer, Number, ParamOwner, ParamRule, ParamSchema, Unit, Unsigned, Value};
    fn contract(control: ParamControl, unit: Unit) -> Contract {
        let mut contract = Contract::from_schema(
            "fixture.kind",
            ParamOwner::Node,
            ParamSchema {
                params: vec![ParamSpec {
                    name: "value".into(),
                    label: "Same label".into(),
                    description: None,
                    control,
                    required: false,
                    default: None,
                    visible_if: None,
                }],
            },
        );
        contract.rules.push(ParamRule { unit, ..ParamRule::implied("value") });
        contract
    }
    #[test]
    fn domains_intersect_ranges_choices_and_full_width_step_lattices() {
        let a = contract(ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(40)), step: Unsigned(4) }, Unit::Count);
        let b = contract(ParamControl::Unsigned { min: Some(Unsigned(3)), max: Some(Unsigned(30)), step: Unsigned(6) }, Unit::Count);
        let specs = shared_specs(&[&a, &b]);
        assert_eq!(specs[0].control, ParamControl::Unsigned { min: Some(Unsigned(9)), max: Some(Unsigned(21)), step: Unsigned(12) });
        for value in [9, 21] {
            for c in [&a, &b] {
                c.schema.params[0].control.validate_value(&Value::Unsigned(Unsigned(value))).unwrap();
            }
        }
        let incompatible = contract(ParamControl::Unsigned { min: Some(Unsigned(2)), max: None, step: Unsigned(4) }, Unit::Count);
        assert!(shared_specs(&[&a, &incompatible]).is_empty());
        assert_eq!(
            progression(u128::from(u64::MAX) - 1, u128::from(u64::MAX), 0, u64::MAX, 1, u64::MAX - 1),
            Some((u128::from(u64::MAX), u128::from(u64::MAX), 1))
        );
        let a = contract(ParamControl::Integer { min: Some(Integer(-9)), max: Some(Integer(30)), step: Unsigned(4) }, Unit::Count);
        let b = contract(ParamControl::Integer { min: Some(Integer(-7)), max: Some(Integer(20)), step: Unsigned(6) }, Unit::Count);
        assert_eq!(shared_specs(&[&a, &b])[0].control, ParamControl::Integer { min: Some(Integer(-1)), max: Some(Integer(11)), step: Unsigned(12) });
        let choices = |values: &[&str]| {
            contract(
                ParamControl::Choice { options: values.iter().map(|value| Choice { value: (*value).into(), label: (*value).into() }).collect() },
                Unit::None,
            )
        };
        let (a, b) = (choices(&["a", "b"]), choices(&["b", "c"]));
        assert_eq!(shared_specs(&[&a, &b])[0].control, ParamControl::Choice { options: vec![Choice { value: "b".into(), label: "b".into() }] });
        let slider = |lo, hi| {
            contract(
                ParamControl::Slider { min: Some(Number::new(lo).unwrap()), max: Some(Number::new(hi).unwrap()), step: Number::new(0.1).unwrap() },
                Unit::Seconds,
            )
        };
        assert!(shared_specs(&[&slider(0., 1.), &slider(2., 3.)]).is_empty());
    }
    #[test]
    fn matching_labels_do_not_override_units_or_meaning_and_defaults_may_differ() {
        let a = contract(ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) }, Unit::Frames);
        let b = contract(a.schema.params[0].control.clone(), Unit::Seconds);
        assert!(shared_specs(&[&a, &b]).is_empty());
        let mut b = a.clone();
        b.rules[0].meaning = Some("different".into());
        assert!(shared_specs(&[&a, &b]).is_empty());
        let mut b = a.clone();
        b.schema.params[0].label = "Different presentation".into();
        b.schema.params[0].default = Some(Value::Unsigned(Unsigned(8)));
        let specs = shared_specs(&[&a, &b]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].default, None);
    }
    #[test]
    fn integer_domains_preserve_endpoints_negative_anchors_and_large_coprime_singletons() {
        assert_eq!(progression(0, u128::from(u64::MAX), 0, u64::MAX, 0, u64::MAX - 1), Some((0, 0, 1)), "LCM above i128 retains representable zero");
        assert_eq!(progression(u128::from(u64::MAX), u128::from(u64::MAX), 0, 1, 0, 1), Some((u128::from(u64::MAX), u128::from(u64::MAX), 1)));
        assert_eq!(progression(7, 7, 1, 3, 3, 4), Some((7, 7, 1)));
        assert_eq!(progression(8, 8, 1, 3, 3, 4), None);
        let control = |min, max, step| contract(ParamControl::Integer { min, max, step: Unsigned(step) }, Unit::Count);
        let a = control(None, None, 1);
        let b = control(Some(Integer(i64::MIN)), Some(Integer(i64::MAX)), 1);
        assert_eq!(shared_specs(&[&a, &b])[0].control, ParamControl::Integer { min: Some(Integer(i64::MIN)), max: Some(Integer(i64::MAX)), step: Unsigned(1) });
        let a = control(Some(Integer(-11)), Some(Integer(10)), 4);
        let b = control(Some(Integer(-9)), Some(Integer(10)), 6);
        assert_eq!(shared_specs(&[&a, &b])[0].control, ParamControl::Integer { min: Some(Integer(-3)), max: Some(Integer(9)), step: Unsigned(12) });
        let exact = 9_007_199_254_740_993_i64;
        let a = control(Some(Integer(exact)), Some(Integer(exact + 2)), 2);
        let b = control(Some(Integer(exact + 2)), Some(Integer(exact + 2)), 1);
        assert_eq!(
            shared_specs(&[&a, &b])[0].control,
            ParamControl::Integer { min: Some(Integer(exact + 2)), max: Some(Integer(exact + 2)), step: Unsigned(1) }
        );
        let unsigned =
            |anchor, step| contract(ParamControl::Unsigned { min: Some(Unsigned(anchor)), max: Some(Unsigned(u64::MAX)), step: Unsigned(step) }, Unit::Count);
        let a = unsigned(0, u64::MAX);
        let b = unsigned(1, u64::MAX - 1);
        let c = unsigned(u64::MAX, 1);
        assert_eq!(
            shared_specs(&[&a, &b, &c])[0].control,
            ParamControl::Unsigned { min: Some(Unsigned(u64::MAX)), max: Some(Unsigned(u64::MAX)), step: Unsigned(1) },
            "three-target fold retains a singleton instead of overflowing a growing LCM"
        );
    }
    #[test]
    fn progression_matches_exhaustive_small_domains() {
        for step_a in 1..=9_u64 {
            for step_b in 1..=9_u64 {
                for a in 0..=9_u128 {
                    for b in 0..=9_u128 {
                        let expected: Vec<_> = (3..=39_u128)
                            .filter(|v| v % u128::from(step_a) == a % u128::from(step_a) && v % u128::from(step_b) == b % u128::from(step_b))
                            .collect();
                        let actual: Vec<_> = match progression(3, 39, a, step_a, b, step_b) {
                            None => vec![],
                            Some((first, last, step)) => (3..=39_u128).filter(|v| *v >= first && *v <= last && (*v - first) % u128::from(step) == 0).collect(),
                        };
                        assert_eq!(actual, expected, "anchors {a}/{b}, steps {step_a}/{step_b}");
                    }
                }
            }
        }
    }
}
