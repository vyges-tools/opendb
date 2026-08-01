// SPDX-License-Identifier: Apache-2.0
//! YAML → the typed model, with every failure located.
use super::model::*;
use super::preprocess::{expand_defines, relative_to};
use std::path::Path;
use yaml_rust2::{yaml::Yaml, YamlLoader};

fn load_yaml(file: &str, text: &str) -> Result<Yaml, BloxError> {
    let docs = YamlLoader::load_from_str(text)
        .map_err(|e| err(file, "", format!("not valid YAML after preprocessing: {e}")))?;
    docs.into_iter().next().ok_or_else(|| err(file, "", "file is empty"))
}

fn f64_of(y: &Yaml) -> Option<f64> {
    match y {
        Yaml::Real(_) => y.as_f64(),
        Yaml::Integer(i) => Some(*i as f64),
        // upstream's own files quote some scalars, so a numeric string is a number here
        Yaml::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// YAML scalars are untyped enough that `2.5` and `"1.0"` are different variants; the version is
/// compared as text, so normalise rather than compare across variants.
fn scalar_text(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Real(r) => Some(r.clone()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn pair(file: &str, path: &str, y: &Yaml) -> Result<(f64, f64), BloxError> {
    let v = y.as_vec().ok_or_else(|| err(file, path, "expected a [x, y] pair"))?;
    if v.len() != 2 {
        return Err(err(file, path, format!("expected 2 values, got {}", v.len())));
    }
    let (a, b) = (f64_of(&v[0]), f64_of(&v[1]));
    match (a, b) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err(err(file, path, "pair values must be numbers")),
    }
}

fn header(file: &str, doc: &Yaml, known: &[&str], kind: &str) -> Result<Header, BloxError> {
    let h = &doc["Header"];
    if h.is_badvalue() {
        return Err(err(file, "Header", "missing"));
    }
    let version = scalar_text(&h["version"]).unwrap_or_default();
    if !known.contains(&version.as_str()) {
        // Refuse rather than parse. A format that moved under us would otherwise be read with
        // the old meaning and no complaint, which is the failure this reader is built to avoid.
        return Err(err(
            file,
            "Header.version",
            format!(
                "{kind} version `{version}` has not been validated by this reader (known: {}). \
                 Refusing rather than risk reading it with the wrong meaning.",
                known.join(", ")
            ),
        ));
    }
    let precision = h["precision"].as_i64().unwrap_or(0) as i32;
    if precision <= 0 {
        return Err(err(file, "Header.precision", "missing or not a positive integer"));
    }
    let includes = h["include"]
        .as_vec()
        .map(|v| v.iter().filter_map(scalar_text).collect())
        .unwrap_or_default();
    Ok(Header {
        version,
        unit: scalar_text(&h["unit"]).unwrap_or_default(),
        precision,
        includes,
    })
}

/// Parse a `.3dbv` — chiplet definitions.
pub fn parse_dbv(file: &str, raw: &str) -> Result<Dbv, BloxError> {
    let doc = load_yaml(file, &expand_defines(raw))?;
    let header = header(file, &doc, KNOWN_DBV_VERSIONS, "3dbv")?;
    let mut chiplets = Vec::new();
    let defs = doc["ChipletDef"]
        .as_hash()
        .ok_or_else(|| err(file, "ChipletDef", "missing or not a mapping"))?;
    for (k, v) in defs {
        let name = scalar_text(k).ok_or_else(|| err(file, "ChipletDef", "non-scalar key"))?;
        let at = format!("ChipletDef.{name}");
        let mut regions = Vec::new();
        if let Some(rs) = v["regions"].as_hash() {
            for (rk, rv) in rs {
                let rname = scalar_text(rk)
                    .ok_or_else(|| err(file, &at, "non-scalar region key"))?;
                let rat = format!("{at}.regions.{rname}");
                let side = scalar_text(&rv["side"])
                    .ok_or_else(|| err(file, &rat, "missing `side`"))?;
                let mut coords = Vec::new();
                if let Some(cs) = rv["coords"].as_vec() {
                    for (i, c) in cs.iter().enumerate() {
                        coords.push(pair(file, &format!("{rat}.coords[{i}]"), c)?);
                    }
                }
                regions.push(Region { name: rname, side, coords });
            }
        }
        chiplets.push(ChipletDef {
            name,
            chip_type: scalar_text(&v["type"]).unwrap_or_else(|| "die".into()),
            design_area: v["design_area"]
                .as_vec()
                .map(|_| pair(file, &format!("{at}.design_area"), &v["design_area"]))
                .transpose()?,
            thickness: f64_of(&v["thickness"]),
            tsv: v["tsv"].as_bool().unwrap_or(false),
            regions,
        });
    }
    Ok(Dbv { header, chiplets })
}

/// Parse a `.3dbx` — the assembly.
pub fn parse_dbx(file: &str, raw: &str) -> Result<Dbx, BloxError> {
    let doc = load_yaml(file, &expand_defines(raw))?;
    let header = header(file, &doc, KNOWN_DBX_VERSIONS, "3dbx")?;
    let design_name = scalar_text(&doc["Design"]["name"])
        .ok_or_else(|| err(file, "Design.name", "missing"))?;

    // ChipletInst names the references; Stack places them. Neither is complete alone, and a
    // name in one but not the other is a real modelling error rather than a default.
    let insts_h = doc["ChipletInst"]
        .as_hash()
        .ok_or_else(|| err(file, "ChipletInst", "missing or not a mapping"))?;
    let mut insts = Vec::new();
    for (k, v) in insts_h {
        let name = scalar_text(k).ok_or_else(|| err(file, "ChipletInst", "non-scalar key"))?;
        let at = format!("ChipletInst.{name}");
        let reference = scalar_text(&v["reference"])
            .ok_or_else(|| err(file, &at, "missing `reference`"))?;
        let s = &doc["Stack"][name.as_str()];
        if s.is_badvalue() {
            return Err(err(file, &format!("Stack.{name}"), "instance has no placement"));
        }
        let sat = format!("Stack.{name}");
        insts.push(ChipletInst {
            name,
            reference,
            placement: Placement {
                loc: pair(file, &format!("{sat}.loc"), &s["loc"])?,
                z: f64_of(&s["z"]).unwrap_or(0.0),
                orient: scalar_text(&s["orient"]).unwrap_or_else(|| "R0".into()),
            },
        });
    }

    let mut connections = Vec::new();
    if let Some(cs) = doc["Connection"].as_hash() {
        for (k, v) in cs {
            let name = scalar_text(k).ok_or_else(|| err(file, "Connection", "non-scalar key"))?;
            let at = format!("Connection.{name}");
            let top_s = scalar_text(&v["top"])
                .ok_or_else(|| err(file, &at, "missing `top`"))?;
            // `bot: ~` is a deliberate virtual bond, not an omission — distinguish the two.
            let bot = match &v["bot"] {
                Yaml::Null | Yaml::BadValue => None,
                other => Some(RegionRef::parse(
                    file,
                    &format!("{at}.bot"),
                    &scalar_text(other).ok_or_else(|| err(file, &at, "`bot` is not a scalar"))?,
                )?),
            };
            connections.push(Connection {
                name,
                top: RegionRef::parse(file, &format!("{at}.top"), &top_s)?,
                bot,
                thickness: f64_of(&v["thickness"]).unwrap_or(0.0),
            });
        }
    }

    let includes = header
        .includes
        .iter()
        .map(|i| relative_to(Path::new(file), i).to_string_lossy().into_owned())
        .collect();
    Ok(Dbx { header, design_name, insts, connections, includes })
}
