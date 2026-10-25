@group(0) @binding(0) var<uniform> light: LightCamera;
@vertex fn vs_main(@location(0) position: vec3<f32>, @location(6) instance: vec4<f32>) -> @builtin(position) vec4<f32> {
    let p = rotate_y(position, instance.w) + instance.xyz;
    return light.view_proj * vec4<f32>(p, 1.0);
}
