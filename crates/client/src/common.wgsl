struct LightCamera { view_proj: mat4x4<f32> };
fn rotate_y(p: vec3<f32>, yaw: f32) -> vec3<f32> {
    let c = cos(yaw); let s = sin(yaw);
    return vec3<f32>(c*p.x + s*p.z, p.y, -s*p.x + c*p.z);
}
struct Camera {
    view_proj: mat4x4<f32>,
    light_view_proj: mat4x4<f32>,
    eye_time: vec4<f32>,
    sun: vec4<f32>,
    fog: vec4<f32>,
    points: array<vec4<f32>, 8>,
    point_colors: array<vec4<f32>, 8>,
    local_view_proj: array<mat4x4<f32>, 8>,
};
// Camera and lighting bindings follow the reusable types.
@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;
@group(1) @binding(2) var fixture_shadows: texture_depth_2d_array;

// Depth changes across a sloping receiver. Comparing every PCF tap against
// the center depth stripes the surface; project its tangent plane instead.
fn depth_gradient(matrix: mat4x4<f32>, clip: vec4<f32>, normal: vec3<f32>) -> vec2<f32> {
    let axis = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.9);
    let tangent = normalize(cross(normal, axis));
    let bitangent = cross(normal, tangent);
    let a = matrix * vec4<f32>(tangent, 0.0);
    let b = matrix * vec4<f32>(bitangent, 0.0);
    let ndc = clip.xyz / clip.w;
    let da = (a.xyz - ndc * a.w) / clip.w;
    let db = (b.xyz - ndc * b.w) / clip.w;
    let ua = da.xy * vec2<f32>(0.5, -0.5);
    let ub = db.xy * vec2<f32>(0.5, -0.5);
    let det = ua.x * ub.y - ua.y * ub.x;
    if (abs(det) < 0.00000001) { return vec2<f32>(0.0); }
    return vec2<f32>(da.z * ub.y - db.z * ua.y, ua.x * db.z - ub.x * da.z) / det;
}
fn fixture_visibility(index: i32, position: vec3<f32>, normal: vec3<f32>) -> f32 {
    let clip = camera.local_view_proj[index] * vec4<f32>(position + normal * 0.015, 1.0);
    if (clip.w <= 0.0) { return 0.0; }
    let ndc = clip.xyz / clip.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if (any(uv <= vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) || ndc.z <= 0.0 || ndc.z >= 1.0) {
        return 0.0;
    }
    let texel = 1.0 / vec2<f32>(textureDimensions(fixture_shadows));
    let gradient = depth_gradient(camera.local_view_proj[index], clip, normal);
    // Linear filtering compares four neighboring texels at each tap.
    let bias = 0.00004 + dot(abs(gradient), texel) * 0.5;
    var sum = 0.0;
    for (var x = -1; x <= 1; x += 1) {
        for (var y = -1; y <= 1; y += 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            sum += textureSampleCompareLevel(fixture_shadows, shadow_sampler,
                uv + offset, index, ndc.z + dot(gradient, offset) - bias);
        }
    }
    return sum / 9.0;
}

fn local_attenuation(i: i32, position: vec3<f32>) -> vec4<f32> {
    let delta = camera.points[i].xyz - position;
    let d2 = max(dot(delta, delta), 0.1);
    let direction = delta * inverseSqrt(d2);
    let range = camera.point_colors[i].w;
    let falloff = max(1.0 - d2 / max(range * range, 0.0001), 0.0);
    let cone = smoothstep(0.5, 0.65, direction.y);
    return vec4<f32>(direction, camera.points[i].w * falloff * falloff * cone / (1.0 + d2));
}
fn apply_fog(color: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    let distance = length(camera.eye_time.xyz - position);
    return mix(color, camera.fog.rgb, clamp(1.0 - exp(-distance * distance * camera.fog.w), 0.0, 0.85));
}
