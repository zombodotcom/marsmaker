// MarsMaker unlit effects (dust puffs, tire tracks, boost flame): StandardMaterial extension that
// does exactly what an `unlit: true` StandardMaterial does (base colour x texture, fog, alpha),
// but without compiling the PBR lighting and shadow code the stock fragment shader carries for
// its runtime unlit switch. WebGL links each such pipeline synchronously, so this matters.
import bevy_pbr::render::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
};

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material.flags, pbr_input.material.alpha_cutoff, pbr_input.material.base_color);
    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, pbr_input.material.base_color);
    return out;
}
