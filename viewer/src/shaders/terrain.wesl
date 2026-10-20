// MarsMaker terrain: StandardMaterial extension.
//
// The HiRISE ortho is greyscale (RED band). Instead of one flat tint, the grey value is mapped
// through a Mars palette (dark basaltic sand -> brown sand -> bright butterscotch dust), so dark
// dunes and bright dust read as different materials while the photo detail is kept as-is.
// Up close a cosmetic sand-ripple normal and a fine grain break up the 0.25-1 m/px texture.
import bevy_pbr::render::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
};

struct TerrainParams {
    // x: ripple strength (0 = off), y: detail fade distance (m), z: grain strength, w: unused
    detail: vec4<f32>,
    // palette stops in sRGB; w = stop position on the 0..1 brightness axis
    c0: vec4<f32>,
    c1: vec4<f32>,
    c2: vec4<f32>,
    c3: vec4<f32>,
    // x: brightness gain, y: saturation, z: wind direction (rad), w: unused
    grade: vec4<f32>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> terrain: TerrainParams;

fn hash_u(x: u32) -> u32 {
    var v = x * 747796405u + 2891336453u;
    v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
    return (v >> 22u) ^ v;
}

fn hash2(p: vec2<i32>) -> f32 {
    let h = hash_u(bitcast<u32>(p.x) ^ hash_u(bitcast<u32>(p.y) + 0x9e3779b9u));
    return f32(h & 0xffffu) / 65535.0;
}

// value noise, 0..1
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let ii = vec2<i32>(i);
    let a = hash2(ii);
    let b = hash2(ii + vec2<i32>(1, 0));
    let c = hash2(ii + vec2<i32>(0, 1));
    let d = hash2(ii + vec2<i32>(1, 1));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn palette(v: f32) -> vec3<f32> {
    let x = clamp(v, 0.0, 1.0);
    var c: vec3<f32>;
    if x < terrain.c1.w {
        c = mix(terrain.c0.rgb, terrain.c1.rgb, (x - terrain.c0.w) / max(terrain.c1.w - terrain.c0.w, 1e-4));
    } else if x < terrain.c2.w {
        c = mix(terrain.c1.rgb, terrain.c2.rgb, (x - terrain.c1.w) / max(terrain.c2.w - terrain.c1.w, 1e-4));
    } else {
        c = mix(terrain.c2.rgb, terrain.c3.rgb, clamp((x - terrain.c2.w) / max(terrain.c3.w - terrain.c2.w, 1e-4), 0.0, 1.0));
    }
    return c;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let wp = in.world_position.xyz;
    let dist = length(view.world_position.xyz - wp);
    let q = wp.xz;

    // brightness of the photo (base colour is white * grey texture), perceptual
    let g = max(pbr_input.material.base_color.r, 0.0);
    var v = pow(g, 1.0 / 2.2);

    // near detail: fine grain and ripples, faded out with distance
    let fade = 1.0 - smoothstep(terrain.detail.y * 0.35, terrain.detail.y, dist);
    let grain_fade = 1.0 - smoothstep(4.0, 45.0, dist);
    let n0 = vnoise(q * 3.7) - 0.5;
    let n1 = vnoise(q * 13.0 + vec2<f32>(17.0, 3.0)) - 0.5;
    let n2 = vnoise(q * 0.35 + vec2<f32>(-5.0, 11.0)) - 0.5;
    v *= 1.0 + terrain.detail.z * grain_fade * (0.55 * n0 + 0.45 * n1);
    v *= 1.0 + 0.05 * n2; // broad, low-contrast patchiness at any distance

    var N = pbr_input.N;
    if terrain.detail.x > 0.0 && fade > 0.0 {
        // wind ripples: crests across the wind, wavy, asymmetric (gentle stoss, steep lee)
        let a = terrain.grade.z;
        let dir = vec2<f32>(cos(a), sin(a));
        let warp = vnoise(q * 0.21) * 7.0 + vnoise(q * 0.9) * 1.6;
        let k1 = 6.2831853 / 1.15;
        let k2 = 6.2831853 / 0.33;
        let p1 = dot(q, dir) * k1 + warp;
        let p2 = dot(q, vec2<f32>(dir.y, -dir.x) * 0.3 + dir) * k2 + warp * 2.3;
        // height h = A1 (sin p + 0.3 sin 2p) + A2 sin p2 ; gradient along dir
        let d1 = (cos(p1) + 0.6 * cos(2.0 * p1)) * k1 * 0.012;
        let d2 = cos(p2) * k2 * 0.002;
        // ripples fade on steep slip faces (avalanching sand) and on flat bright dust
        let slope_mask = smoothstep(0.55, 0.85, pbr_input.world_normal.y);
        let sand_mask = 1.0 - 0.6 * smoothstep(0.55, 0.85, v);
        let s = terrain.detail.x * fade * slope_mask * sand_mask;
        let grad = dir * (d1 + d2) * s;
        N = normalize(N - vec3<f32>(grad.x, 0.0, grad.y));
        // coarse grains collect on ripple crests: slightly brighter crests
        v *= 1.0 + 0.06 * s * sin(p1);
    }
    pbr_input.N = N;

    var rgb_srgb = palette(v * terrain.grade.x);
    // saturation trim
    let l = dot(rgb_srgb, vec3<f32>(0.299, 0.587, 0.114));
    rgb_srgb = mix(vec3<f32>(l), rgb_srgb, terrain.grade.y);
    let rgb = pow(max(rgb_srgb, vec3<f32>(0.0)), vec3<f32>(2.2));
    pbr_input.material.base_color = vec4<f32>(rgb, 1.0);
    // dark, coarse basaltic sand is a touch less rough than fine bright dust
    pbr_input.material.perceptual_roughness = mix(0.86, 0.97, smoothstep(0.2, 0.7, v));

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
