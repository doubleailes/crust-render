use crate::prim::UsdPrim;

pub fn parse_usda(text: &str) -> Vec<UsdPrim> {
    let mut prims = Vec::new();
    for block in text.split("def ") {
        if block.contains("Sphere") {
            let name = block
                .lines()
                .next()
                .and_then(|line| line.split('"').nth(1))
                .unwrap_or("Unnamed")
                .to_string();

            let radius = parse_f32_attr(block, "radius").unwrap_or(1.0);
            let translate = parse_vec3_attr(block, "xformOp:translate").unwrap_or((0.0, 0.0, 0.0));

            prims.push(UsdPrim::Sphere {
                name,
                radius,
                position: translate,
            });
        }
    }
    prims
}

fn parse_f32_attr(block: &str, key: &str) -> Option<f32> {
    block
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("float {}", key)))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|val| val.trim().trim_end_matches(';').parse().ok())
}

fn parse_vec3_attr(block: &str, key: &str) -> Option<(f32, f32, f32)> {
    block
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("double3 {}", key)))
        .and_then(|l| l.split('=').nth(1))
        .map(|val| {
            val.trim_matches(|c| c == '(' || c == ')' || c == ';')
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect::<Vec<_>>()
        })
        .and_then(|v| {
            if v.len() == 3 {
                Some((v[0], v[1], v[2]))
            } else {
                None
            }
        })
}
