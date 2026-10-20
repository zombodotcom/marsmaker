// Browser build only: bevy_pbr 0.20.0 src/render/shadows.wesl (MIT OR Apache-2.0, (c) the Bevy
// contributors), trimmed so WebGL shader programs link in about a second instead of ~7 s each.
// Chrome on Windows compiles WebGL through ANGLE -> D3D11, and every StandardMaterial pipeline
// inlined the point/spot light cube-map shadow filters and the PCSS soft-shadow blocker search;
// each link blocked the page (and Chrome's GPU process) for seconds. The viewer has no shadowed
// point/spot lights and no soft shadows, so those paths return "lit" here. Swapped in by
// visuals::lite_web_shaders; must be refreshed when bevy is upgraded.
import package::render::{
    mesh_view_types::POINT_LIGHT_FLAGS_SPOT_LIGHT_Y_NEGATIVE,
    mesh_view_bindings as view_bindings,
    shadow_sampling::{
        SPOT_SHADOW_TEXEL_SIZE, sample_shadow_map,
    }
};

import bevy_render::{
    color_operations::hsv_to_rgb,
    maths::{orthonormalize, PI_2}
};

const flip_z: vec3<f32> = vec3<f32>(1.0, 1.0, -1.0);

fn fetch_point_shadow(
    light_id: u32,
    frag_position: vec4<f32>,
    surface_normal: vec3<f32>,
    frag_coord_xy: vec2<f32>,
) -> f32 {
    return 1.0;
}

fn fetch_spot_shadow(
    light_id: u32,
    frag_position: vec4<f32>,
    surface_normal: vec3<f32>,
    near_z: f32,
    frag_coord_xy: vec2<f32>,
) -> f32 {
    return 1.0;
}

fn get_cascade_index(light_id: u32, view_z: f32) -> u32 {
    let light = &view_bindings::lights.directional_lights[light_id];

    for (var i: u32 = 0u; i < (*light).num_cascades; i = i + 1u) {
        if (-view_z < (*light).cascades[i].far_bound) {
            return i;
        }
    }
    return (*light).num_cascades;
}

/// Converts from world space to the uv position in the light's shadow map.
///
/// The depth is stored in the return value's z coordinate. If the return value's
/// w coordinate is 0.0, then we landed outside the shadow map entirely.
fn world_to_directional_light_local(
    light_id: u32,
    cascade_index: u32,
    offset_position: vec4<f32>
) -> vec4<f32> {
    let light = &view_bindings::lights.directional_lights[light_id];
    let cascade = &(*light).cascades[cascade_index];

    let offset_position_clip = (*cascade).clip_from_world * offset_position;
    if (offset_position_clip.w <= 0.0) {
        return vec4(0.0);
    }
    let offset_position_ndc = offset_position_clip.xyz / offset_position_clip.w;
    // No shadow outside the orthographic projection volume
    if (any(offset_position_ndc.xy < vec2<f32>(-1.0)) || offset_position_ndc.z < 0.0
            || any(offset_position_ndc > vec3<f32>(1.0))) {
        return vec4(0.0);
    }

    // compute texture coordinates for shadow lookup, compensating for the Y-flip difference
    // between the NDC and texture coordinates
    let flip_correction = vec2<f32>(0.5, -0.5);
    let light_local = offset_position_ndc.xy * flip_correction + vec2<f32>(0.5, 0.5);

    let depth = offset_position_ndc.z;

    return vec4(light_local, depth, 1.0);
}

fn sample_directional_cascade(
    light_id: u32,
    cascade_index: u32,
    frag_position: vec4<f32>,
    surface_normal: vec3<f32>,
    frag_coord_xy: vec2<f32>,
) -> f32 {
    let light = &view_bindings::lights.directional_lights[light_id];
    let cascade = &(*light).cascades[cascade_index];

    // The normal bias is scaled to the texel size.
    let normal_offset = (*light).shadow_normal_bias * (*cascade).texel_size * surface_normal.xyz;
    let depth_offset = (*light).shadow_depth_bias * (*light).direction_to_light.xyz;
    let offset_position = vec4<f32>(frag_position.xyz + normal_offset + depth_offset, frag_position.w);

    let light_local = world_to_directional_light_local(light_id, cascade_index, offset_position);
    if (light_local.w == 0.0) {
        return 1.0;
    }

    let array_index = i32((*light).depth_texture_base_index + cascade_index);
    let texel_size = (*cascade).texel_size;

    return sample_shadow_map(
        light_local.xy,
        light_local.z,
        array_index,
        frag_coord_xy,
        texel_size,
    );
}

fn fetch_directional_shadow(
    light_id: u32,
    frag_position: vec4<f32>,
    surface_normal: vec3<f32>,
    view_z: f32,
    frag_coord_xy: vec2<f32>,
) -> f32 {
    let light = &view_bindings::lights.directional_lights[light_id];
    let cascade_index = get_cascade_index(light_id, view_z);

    if (cascade_index >= (*light).num_cascades) {
        return 1.0;
    }

    var shadow = sample_directional_cascade(
        light_id,
        cascade_index,
        frag_position,
        surface_normal,
        frag_coord_xy,
    );

    // Blend with the next cascade, if there is one.
    let next_cascade_index = cascade_index + 1u;
    if (next_cascade_index < (*light).num_cascades) {
        let this_far_bound = (*light).cascades[cascade_index].far_bound;
        let next_near_bound = (1.0 - (*light).cascades_overlap_proportion) * this_far_bound;
        if (-view_z >= next_near_bound) {
            let next_shadow = sample_directional_cascade(
                light_id,
                next_cascade_index,
                frag_position,
                surface_normal,
                frag_coord_xy,
            );
            shadow = mix(shadow, next_shadow, (-view_z - next_near_bound) / (this_far_bound - next_near_bound));
        }
    }
    return shadow;
}

fn cascade_debug_visualization(
    output_color: vec3<f32>,
    light_id: u32,
    view_z: f32,
) -> vec3<f32> {
    let overlay_alpha = 0.95;
    let cascade_index = get_cascade_index(light_id, view_z);
    let cascade_color_hsv = vec3(
        f32(cascade_index) / f32(u32(constants::MAX_CASCADES_PER_LIGHT) + 1u) * PI_2,
        1.0,
        0.5
    );
    let cascade_color = hsv_to_rgb(cascade_color_hsv);
    return vec3<f32>(
        (1.0 - overlay_alpha) * output_color.rgb + overlay_alpha * cascade_color
    );
}
