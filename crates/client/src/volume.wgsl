struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) size: vec2<f32>,
    @location(4) material: u32,
};
struct VertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) @interpolate(flat) size: vec2<f32>,
    @location(4) @interpolate(flat) material: u32,
};

@vertex fn vs_main(v: VertexInput) -> VertexOutput {
    var o: VertexOutput;
    o.clip = camera.view_proj * vec4<f32>(v.position, 1.0);
    o.position = v.position;
    o.normal = v.normal;
    o.uv = v.uv;
    o.size = v.size;
    o.material = v.material;
    return o;
}

@fragment fn fs_main(v: VertexOutput, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let water = v.material == 0u;
    let geometric = normalize(v.normal);
    let view = normalize(camera.eye_time.xyz - v.position);
    let facing = select(-geometric, geometric, front);
    var n = facing;
    if (water) {
        // Animate only the normal, keeping authored water edges watertight and
        // avoiding vertex tessellation proportional to the size of a lake.
        let axis = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.9);
        let tangent = normalize(cross(axis, n));
        let bitangent = cross(n, tangent);
        let p = vec2<f32>(dot(v.position, tangent), dot(v.position, bitangent));
        let time = camera.eye_time.w;
        let a = dot(p, vec2<f32>(0.72, 0.39)) + time * 0.63;
        let b = dot(p, vec2<f32>(-1.38, 1.16)) - time * 0.91;
        let c = dot(p, vec2<f32>(2.41, 1.67)) + time * 1.21;
        let slope = vec2<f32>(0.72, 0.39) * cos(a) * 0.055
            + vec2<f32>(-1.38, 1.16) * cos(b) * 0.026
            + vec2<f32>(2.41, 1.67) * cos(c) * 0.009;
        n = normalize(n - tangent * slope.x - bitangent * slope.y);
    }
    let cosine = clamp(abs(dot(n, view)), 0.0, 1.0);
    let fresnel = pow(1.0 - cosine, 5.0);
    let reflection = reflect(-view, n);
    // A stable sky-colored sheen needs no scene copy or screen-space tracing.
    // This is deliberately a stylized surface, not a reflection of geometry.
    let environment = mix(vec3<f32>(0.12, 0.16, 0.18), vec3<f32>(0.30, 0.38, 0.43),
        clamp(reflection.y * 0.5 + 0.5, 0.0, 1.0));
    let edge_distance = min(min(v.uv.x, v.uv.y), min(v.size.x - v.uv.x, v.size.y - v.uv.y));
    let edge = 1.0 - smoothstep(0.0, max(0.024, fwidth(edge_distance) * 1.25), edge_distance);
    let sun = normalize(camera.sun.xyz);
    let half_direction = normalize(sun + view);
    let specular = pow(max(dot(n, half_direction), 0.0), select(180.0, 100.0, water));
    var color = mix(vec3<f32>(0.19, 0.32, 0.34), environment, 0.65 + fresnel * 0.30);
    var alpha = 0.075 + fresnel * 0.43 + edge * 0.24;
    if (water) {
        color = mix(vec3<f32>(0.025, 0.19, 0.235), environment, fresnel * 0.75);
        let flow = sin(v.position.x * 0.48 + v.position.z * 0.61 + camera.eye_time.w * 0.32);
        color *= 0.94 + 0.06 * flow;
        alpha = 0.18 + fresnel * 0.46;
    }
    alpha = clamp(alpha, 0.0, 0.76);
    // Highlights come from the sun and from the nearest lights, and are added
    // after the surface color is weighted by its opacity, so glass and water
    // keep bright glints however clear they are.
    var surface: Surface;
    surface.position = v.position;
    surface.normal = n;
    surface.geometric = facing;
    surface.view = view;
    surface.specular_power = select(180.0, 100.0, water);
    surface.specular_only = true;
    let local = shade_local_lights(v.clip, surface, min(light_params.mode.w, TRANSPARENT_LIGHTS));
    let glints = vec3<f32>(0.72, 0.76, 0.76) * specular * 0.65 + local.specular * 0.24;
    let fog = fog_factor(v.position);
    return vec4<f32>(mix(color, camera.fog.rgb, fog) * alpha + glints * (1.0 - fog), alpha);
}

struct UnderwaterOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex fn vs_underwater(@builtin(vertex_index) index: u32) -> UnderwaterOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var o: UnderwaterOutput;
    o.position = vec4<f32>(positions[index], 0.0, 1.0);
    o.uv = positions[index] * 0.5 + vec2<f32>(0.5);
    return o;
}
@fragment fn fs_underwater(v: UnderwaterOutput) -> @location(0) vec4<f32> {
    let vignette = clamp(length(v.uv - vec2<f32>(0.5)), 0.0, 0.7);
    return vec4<f32>(vec3<f32>(0.015, 0.115, 0.145), 0.055 + vignette * 0.075);
}
