#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> settings: vec4<f32>;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, is_front);
    // Vertex colors carry normalized material weights, including whole-cell water.
    let weights = in.color / max(dot(in.color, vec4<f32>(1.0)), 0.0001);
    let grass = vec3<f32>(0.23, 0.36, 0.14);
    let forest_floor = vec3<f32>(0.13, 0.23, 0.105);
    let rock = vec3<f32>(0.43, 0.42, 0.38);
    let water = vec3<f32>(0.035, 0.23, 0.36);
    let grain = 0.98 + 0.02 * sin(in.world_position.x * 16.0) * sin(in.world_position.z * 19.0);
    let base = (grass * weights.x + forest_floor * weights.y + rock * weights.z + water * weights.w) * grain;
    pbr.material.base_color = vec4<f32>(base * settings.x, 1.0);
    pbr.material.perceptual_roughness = mix(0.93, 0.3, weights.w);
    pbr.material.metallic = 0.0;
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);
    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
