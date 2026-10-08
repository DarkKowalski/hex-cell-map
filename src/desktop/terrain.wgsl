#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> settings: vec4<f32>;
fn hash(p: vec2<f32>) -> f32 { return fract(sin(dot(p, vec2<f32>(127.1,311.7))) * 43758.5453); }
fn noise(p: vec2<f32>) -> f32 {
    let i=floor(p); let f=fract(p); let u=f*f*(3.0-2.0*f);
    return mix(mix(hash(i),hash(i+vec2<f32>(1.0,0.0)),u.x),mix(hash(i+vec2<f32>(0.0,1.0)),hash(i+vec2<f32>(1.0,1.0)),u.x),u.y);
}
@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr=pbr_input_from_standard_material(in,is_front);
    let p=in.world_position.xz;
    let broad=noise(p*0.65);let medium=noise(p*4.3);let fine=noise(p*34.0);
    var base=in.color.rgb;
    if settings.y > 0.5 {
        let ripple=noise(p*25.0+vec2<f32>(medium*0.08,0.0));
        base=mix(base*vec3<f32>(0.85,0.94,1.05),base*vec3<f32>(1.12,1.15,0.94),broad);
        base*=0.94+0.10*ripple;
        pbr.material.perceptual_roughness=0.32;
    } else {
        // Variation changes tint and grain, not geography or land classification.
        base*=0.82+0.18*broad+0.12*medium+0.06*fine;
        base+=vec3<f32>(0.024,0.009,-0.009)*(broad-0.5);
        let urban=in.color.a;
        // Small street grid sits inside urban coverage and softens at its edge.
        let block=abs(fract(p/0.18)-vec2<f32>(0.5));
        let street=smoothstep(0.44,0.49,max(block.x,block.y))*urban;
        base=mix(base,vec3<f32>(0.115,0.12,0.115),street*0.65);
        pbr.material.perceptual_roughness=0.94;
    }
    pbr.material.base_color=vec4<f32>(max(base,vec3<f32>(0.01)),1.0);
    pbr.material.metallic=0.0;
    var out:FragmentOutput;
    out.color=apply_pbr_lighting(pbr);
    out.color=main_pass_post_lighting_processing(pbr,out.color);
    return out;
}
