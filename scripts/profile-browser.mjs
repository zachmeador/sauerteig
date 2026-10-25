// Use Playwright's isolated Chromium, never the user's installed browser/profile.
// PLAYWRIGHT_MODULE can point to a temporary installation's index.mjs.
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
import fs from 'node:fs/promises';

const [url, destination] = process.argv.slice(2);
if (!url || !destination) {
  throw new Error('Usage: node scripts/profile-browser.mjs URL OUTPUT_PREFIX');
}
const browser = await chromium.launch({ channel: 'chromium', headless: true });
try {
  const page = await browser.newPage({
    viewport: { width: 3840, height: 2160 }, deviceScaleFactor: 1,
  });
  const errors = [];
  await page.route('**/favicon.ico', route => route.fulfill({ status: 204 }));
  page.on('pageerror', error => errors.push(String(error)));
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });
  await page.addInitScript(() => {
    window.renderSamples = [];
    window.drawTimes = [];
    window.gpuErrors = [];
    const requestDevice = GPUAdapter.prototype.requestDevice;
    GPUAdapter.prototype.requestDevice = async function (...args) {
      const device = await requestDevice.apply(this, args);
      device.addEventListener('uncapturederror', event => {
        window.gpuErrors.push(event.error.message);
      });
      return device;
    };
    // Observe the app's own timestamp readbacks. Associate labels with query
    // sets and destination buffers because readbacks complete asynchronously.
    const queryLabels = new WeakMap();
    const encoderLabels = new WeakMap();
    const bufferLabels = new WeakMap();
    const begin = GPUCommandEncoder.prototype.beginRenderPass;
    GPUCommandEncoder.prototype.beginRenderPass = function (descriptor) {
      if (descriptor.label === 'world pass') window.drawTimes.push(performance.now());
      if (descriptor.timestampWrites) {
        const { querySet, beginningOfPassWriteIndex: index } = descriptor.timestampWrites;
        if (index === 0) queryLabels.set(querySet, []);
        queryLabels.get(querySet)[index / 2] = descriptor.label;
      }
      return begin.call(this, descriptor);
    };
    const resolve = GPUCommandEncoder.prototype.resolveQuerySet;
    GPUCommandEncoder.prototype.resolveQuerySet = function (set, first, count, ...rest) {
      encoderLabels.set(this, queryLabels.get(set)?.slice(first / 2, (first + count) / 2));
      return resolve.call(this, set, first, count, ...rest);
    };
    const copy = GPUCommandEncoder.prototype.copyBufferToBuffer;
    GPUCommandEncoder.prototype.copyBufferToBuffer = function (source, offset, target, ...rest) {
      if (target.label === 'timestamp readback') bufferLabels.set(target, encoderLabels.get(this));
      return copy.call(this, source, offset, target, ...rest);
    };
    const mapped = GPUBuffer.prototype.getMappedRange;
    GPUBuffer.prototype.getMappedRange = function (...args) {
      const result = mapped.apply(this, args);
      if (this.label === 'timestamp readback') {
        window.renderSamples.push({
          values: Array.from(new BigUint64Array(result), Number), labels: bufferLabels.get(this),
        });
      }
      return result;
    };
  });
  await page.goto(url);
  await page.waitForSelector('#game-canvas', { timeout: 120000 });
  await page.waitForTimeout(15000);
  // Exercise resize and verify physical resolution, including the baseline's
  // asynchronous initial-size race. Do not benchmark a CSS-stretched 1x1 canvas.
  await page.setViewportSize({ width: 3840, height: 2159 });
  await page.waitForTimeout(500);
  await page.setViewportSize({ width: 3840, height: 2160 });
  await page.waitForTimeout(3000);
  await page.waitForFunction(() => {
    const canvas = document.querySelector('canvas');
    return canvas?.width === 3840 && canvas?.height === 2160;
  });
  await page.keyboard.press('F3');
  await page.evaluate(() => { window.renderSamples = []; window.drawTimes = []; });
  await page.waitForTimeout(5000);
  const result = await page.evaluate(async () => {
    const adapter = await navigator.gpu.requestAdapter();
    const canvas = document.querySelector('canvas');
    return {
      gpu: window.renderSamples, draws: window.drawTimes, gpuErrors: window.gpuErrors,
      canvas: { width: canvas.width, height: canvas.height },
      adapter: adapter ? {
        vendor: adapter.info.vendor, architecture: adapter.info.architecture,
        device: adapter.info.device, description: adapter.info.description,
      } : null,
      error: document.querySelector('#error-message')?.textContent,
    };
  });
  result.errors = errors;
  await fs.writeFile(destination + '.json', JSON.stringify(result, null, 2));
  await page.screenshot({ path: destination + '.png' });
  console.log(JSON.stringify({ samples: result.gpu.length, errors, gpuErrors: result.gpuErrors, adapter: result.adapter }));
  if (errors.length || result.gpuErrors.length || result.error) process.exitCode = 1;
} finally {
  await browser.close();
}
