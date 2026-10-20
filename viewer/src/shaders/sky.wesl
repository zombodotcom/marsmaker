// MarsMaker sky dome: an analytic Mars sky on a big inside-out sphere that follows the camera.
//
// Mars' dusty sky is butterscotch by day, darker towards the zenith, hazy and bright at the
// horizon, with a cool blue-white glow around the sun (forward scattering by ~1.5 um dust),
// strongest near sunset. The horizon colour is the distance-fog colour (set from the same
// numbers on the CPU), so far terrain melts into the sky without a seam.
import bevy_pbr::render::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::main_pass_post_lighting_processing,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::{view, lights},
};

struct SkyParams {
    zenith: vec4<f32>,     // linear rgb; w: horizon band sharpness
    horizon: vec4<f32>,    // linear rgb (= fog colour); w: ground haze darkening below the horizon
    aureole: vec4<f32>,    // linear rgb of the blue glow; w: its width exponent
    fog_sun: vec4<f32>,    // fog directional colour (rgb * a premultiplied); w: fog directional exponent
    sun: vec4<f32>,        // x: disc cos(angle), y: disc intensity, z: halo intensity, w: aureole intensity
};

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> sky: SkyParams;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);

    var to_sun = vec3<f32>(0.0, 1.0, 0.0);
    var sun_rgb = vec3<f32>(1.0);
    if lights.n_directional_lights > 0u {
        to_sun = normalize(lights.directional_lights[0].direction_to_light);
        sun_rgb = lights.directional_lights[0].color.rgb * view.exposure;
    }
    let mu = dot(dir, to_sun);
    let up = dir.y;

    // base gradient: horizon haze -> zenith
    let h = clamp(up, 0.0, 1.0);
    let t = pow(1.0 - h, sky.zenith.w);
    var col = mix(sky.zenith.rgb, sky.horizon.rgb, t);

    // the side of the sky facing the sun is brighter (dust scatters forward)
    let fwd = max(mu, 0.0);
    col *= 1.0 + 0.35 * fwd * fwd;

    // fog-matching sun-side brightening at the horizon (same formula as the distance fog)
    let fog_scatter = pow(fwd, sky.fog_sun.w) * sun_rgb * sky.fog_sun.rgb;
    col += fog_scatter * t;

    // blue aureole and bright halo around the sun
    col += sky.aureole.rgb * (pow(fwd, sky.aureole.w) * sky.sun.w);
    col += vec3<f32>(1.0, 0.97, 0.92) * (pow(fwd, 600.0) * sky.sun.z);

    // the sun disc (0.35 deg across from Mars), soft-edged
    let disc = smoothstep(sky.sun.x - 0.0000015, sky.sun.x + 0.0000015, mu);
    col += vec3<f32>(1.0, 0.98, 0.95) * disc * sky.sun.y;

    // below the horizon (seen from high up): ground haze, slightly darker than the horizon
    if up < 0.0 {
        let below = clamp(-up * 8.0, 0.0, 1.0);
        let haze = sky.horizon.rgb + fog_scatter;
        col = mix(col, haze * (1.0 - sky.horizon.w * below), clamp(-up * 40.0, 0.0, 1.0));
    }

    pbr_input.material.base_color = vec4<f32>(col, 1.0);
    var out: FragmentOutput;
    out.color = vec4<f32>(col, 1.0);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
