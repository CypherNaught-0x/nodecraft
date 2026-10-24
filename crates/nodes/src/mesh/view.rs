//! Saved presentation of a built-in 3D card (#928). These values never describe a generated
//! model or converted asset: their contract excludes them from run identity and pricing.
use nodecraft_types::{Affects, Contract, Error, ErrorKind, Number, ParamControl, ParamRule, ParamSpec, ParamValues, Unit, Value};
use serde::{Deserialize, Serialize};

pub const YAW: &str = "view_yaw";
pub const PITCH: &str = "view_pitch";
pub const ZOOM: &str = "view_zoom";
pub const STYLE: &str = "view_style";
pub const NAMES: [&str; 4] = [YAW, PITCH, ZOOM, STYLE];
pub const MAX_PITCH: f64 = 1.55 * 180.0 / std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    #[default]
    Shaded,
    Unlit,
    Wireframe,
}

impl Style {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shaded => "shaded",
            Self::Unlit => "unlit",
            Self::Wireframe => "wireframe",
        }
    }
}

/// Angles in degrees; zoom 1 fits the model. Missing parameters in older projects use the
/// same three-quarter camera the original viewer used.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub yaw: f64,
    pub pitch: f64,
    pub zoom: f64,
    pub style: Style,
}

impl Default for State {
    fn default() -> Self {
        Self { yaw: 0.6_f64.to_degrees(), pitch: 0.35_f64.to_degrees(), zoom: 1.0, style: Style::Shaded }
    }
}

fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "3D view needs finite angles in range, zoom from 0.25 to 16, and a supported shading mode")
}

impl State {
    pub fn validate(self) -> Result<Self, Error> {
        if !self.yaw.is_finite()
            || !(0.0..=360.0).contains(&self.yaw)
            || !self.pitch.is_finite()
            || !(-MAX_PITCH..=MAX_PITCH).contains(&self.pitch)
            || !self.zoom.is_finite()
            || !(0.25..=16.0).contains(&self.zoom)
        {
            return Err(invalid());
        }
        Ok(self)
    }

    pub fn read(params: &ParamValues) -> Result<Self, Error> {
        let default = Self::default();
        let number = |name: &str, fallback: f64| match params.get(name) {
            None => Ok(fallback),
            Some(Value::Number(n)) => Ok(n.get()),
            _ => Err(invalid()),
        };
        let style = match params.get(STYLE) {
            None => Style::Shaded,
            Some(Value::Text(s)) if s == "shaded" => Style::Shaded,
            Some(Value::Text(s)) if s == "unlit" => Style::Unlit,
            Some(Value::Text(s)) if s == "wireframe" => Style::Wireframe,
            _ => return Err(invalid()),
        };
        Self { yaw: number(YAW, default.yaw)?, pitch: number(PITCH, default.pitch)?, zoom: number(ZOOM, default.zoom)?, style }.validate()
    }

    pub fn params(self) -> Result<ParamValues, Error> {
        self.validate()?;
        Ok([
            (YAW.into(), Value::Number(Number::new(self.yaw)?)),
            (PITCH.into(), Value::Number(Number::new(self.pitch)?)),
            (ZOOM.into(), Value::Number(Number::new(self.zoom)?)),
            (STYLE.into(), Value::Text(self.style.as_str().into())),
        ]
        .into())
    }
}

pub fn supports(kind: &str) -> bool {
    matches!(kind, super::SOURCE | super::CONVERT | crate::generation::MESH_KIND)
}

pub(crate) fn schema() -> Vec<ParamSpec> {
    let defaults = State::default();
    let Ok(step) = Number::new(0.01) else { return Vec::new() };
    let slider = |name: &str, label: &str, min: f64, max: f64, value: f64| ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some("Saved viewer setting; does not change the model or its generation.".into()),
        // All constants are finite. Slider step is only a UI increment, not quantization.
        control: ParamControl::Slider { min: Number::new(min).ok(), max: Number::new(max).ok(), step },
        required: false,
        default: Number::new(value).ok().map(Value::Number),
        visible_if: None,
    };
    vec![
        slider(YAW, "View angle", 0.0, 360.0, defaults.yaw),
        slider(PITCH, "View elevation", -MAX_PITCH, MAX_PITCH, defaults.pitch),
        slider(ZOOM, "View zoom", 0.25, 16.0, defaults.zoom),
        super::choice(STYLE, "View shading", &[("shaded", "Shaded"), ("unlit", "Unlit"), ("wireframe", "Wireframe")], "shaded"),
    ]
}

pub(crate) fn contract(contract: &mut Contract) {
    for name in NAMES {
        if contract.schema.params.iter().any(|p| p.name == name) {
            let rule = ParamRule {
                affects: Affects { identity: false, cost: false, shape: false, jobs: false },
                unit: if name == YAW || name == PITCH { Unit::Degrees } else { Unit::None },
                ..ParamRule::implied(name)
            };
            contract.rules.retain(|r| r.param != name);
            contract.rules.push(rule);
        }
    }
}

pub(crate) fn strip(params: &mut ParamValues) {
    for name in NAMES {
        params.remove(name);
    }
}

/// Remove only the newly introduced node-owned viewer controls from a Generate 3D contract's
/// identity representation. Its older saved generation hashes used this same contract without
/// presentation metadata; adding a viewer must not make those paid results stale on upgrade.
/// Call only on the node-owned parameter group of `generation.mesh`, never on model groups.
pub fn strip_identity_contract(contract: &mut Contract) {
    contract.schema.params.retain(|spec| !NAMES.contains(&spec.name.as_str()));
    contract.rules.retain(|rule| !NAMES.contains(&rule.param.as_str()));
}
