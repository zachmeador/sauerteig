@group(2) @binding(0) var albedo_map: texture_2d_array<f32>;
@group(2) @binding(1) var normal_map: texture_2d_array<f32>;
@group(2) @binding(2) var material_sampler: sampler;
struct Materials { values: array<vec4<f32>,64> };
@group(2) @binding(3) var<uniform> materials: Materials;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) emission: f32,
    @location(4) uv: vec2<f32>,
    @location(5) material: u32,
    @location(6) instance: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) light_clip: vec4<f32>,
    @location(4) emission: f32,
    @location(5) uv: vec2<f32>,
    @location(6) @interpolate(flat) material: u32,
};
@vertex fn vs_main(v: VertexInput) -> VertexOutput {
    let position = rotate_y(v.position,v.instance.w) + v.instance.xyz;
    let normal = rotate_y(v.normal,v.instance.w);
    var o: VertexOutput;
    o.clip = camera.view_proj * vec4<f32>(position, 1.0);
    o.position = position;
    o.normal = normal;
    o.uv = v.uv;
    o.material = v.material;
    o.color = v.color;
    // A small world-space offset avoids self-shadow acne without pushing
    // contact shadows centimetres away as a large depth-space bias did.
    o.light_clip = camera.light_view_proj * vec4<f32>(position + normal * 0.012, 1.0);
    o.emission = v.emission;
    return o;
}
@vertex fn vs_shadow(v: VertexInput) -> @builtin(position) vec4<f32> {
    return camera.light_view_proj * vec4<f32>(rotate_y(v.position,v.instance.w) + v.instance.xyz, 1.0);
}
@fragment fn fs_main(v: VertexOutput) -> @location(0) vec4<f32> {
    let geometric_normal = normalize(v.normal);
    var n = geometric_normal;
    var color = v.color;
    var roughness = 0.8;
    var metallic = 0.0;
    var ao = 1.0;
    var emission = v.emission;
    // Explicit derivatives remain valid across different material triangles.
    let px = dpdx(v.position); let py = dpdy(v.position);
    let tx = dpdx(v.uv); let ty = dpdy(v.uv);
    if (v.material < 64u) {
        let base = textureSampleGrad(albedo_map, material_sampler, v.uv, i32(v.material), tx, ty);
        let detail = textureSampleGrad(normal_map, material_sampler, v.uv, i32(v.material), tx, ty);
        color = base.rgb * v.color;
        ao = base.a;
        roughness = clamp(detail.a, 0.08, 1.0);
        metallic = materials.values[v.material].x;
        emission = max(emission, materials.values[v.material].y);
        let determinant = tx.x * ty.y - tx.y * ty.x;
        if (abs(determinant) > 0.00000001) {
            let tangent = (px * ty.y - py * tx.y) / determinant;
            let bitangent = (py * tx.x - px * ty.x) / determinant;
            let t = normalize(tangent - n * dot(n,tangent));
            let b = normalize(cross(n,t)) * sign(dot(cross(n,t),bitangent));
            n = normalize(mat3x3<f32>(t,b,n) * (detail.xyz * 2.0 - 1.0));
        }
    }
    let specular = mix(vec3<f32>(0.025), color * 0.25, metallic);
    let roughness_power = mix(180.0, 4.0, roughness * roughness);
    let sun = normalize(camera.sun.xyz);
    let light_ndc = v.light_clip.xyz / v.light_clip.w;
    let uv = light_ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    var visibility = 1.0;
    if (all(uv > vec2<f32>(0.0)) && all(uv < vec2<f32>(1.0)) && light_ndc.z > 0.0 && light_ndc.z < 1.0) {
        var sum = 0.0;
        let texel = 1.0 / vec2<f32>(textureDimensions(shadow_map));
        let gradient = depth_gradient(camera.light_view_proj, v.light_clip, geometric_normal);
        let bias = 0.00003 + dot(abs(gradient), texel) * 0.5;
        for (var x = -1; x <= 1; x += 1) {
            for (var y = -1; y <= 1; y += 1) {
                let offset = vec2<f32>(f32(x), f32(y)) * texel;
                sum += textureSampleCompareLevel(shadow_map, shadow_sampler,
                    uv + offset, light_ndc.z + dot(gradient, offset) - bias);
            }
        }
        // Fade the bounded camera-following shadow window before its edge.
        // Distant terrain remains sunlit instead of acquiring a moving hard seam.
        let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
        visibility = mix(1.0, sum / 9.0, smoothstep(0.0, 0.08, edge));
    }
    let ambient = mix(vec3<f32>(0.045, 0.055, 0.070), vec3<f32>(0.105, 0.13, 0.155), n.y * 0.5 + 0.5);
    var lit = color * (ambient * ao + vec3<f32>(0.85, 0.88, 0.90) * max(dot(n, sun), 0.0) * visibility);
    let view = normalize(camera.eye_time.xyz - v.position);
    var surface: Surface;
    surface.position = v.position;
    surface.normal = n;
    surface.geometric = geometric_normal;
    surface.view = view;
    surface.specular_power = roughness_power;
    let local = shade_local_lights(v.clip, surface, light_params.mode.w);
    lit += color * local.diffuse + specular * local.specular;
    let half_vec = normalize(sun + view);
    lit += specular * pow(max(dot(n, half_vec), 0.0), roughness_power) * visibility * max(dot(n, sun), 0.0);
    lit = mix(lit, color * 1.8, clamp(emission, 0.0, 1.0));
    return vec4<f32>(apply_fog(lit, v.position), 1.0);
}
