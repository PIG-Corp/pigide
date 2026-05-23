const { test } = require('@playwright/test');

test('capture screenshots', async ({ page }) => {
  // Set window viewport
  await page.setViewportSize({ width: 1280, height: 900 });
  
  // Navigate to Vite dev server
  console.log("Navigating to http://127.0.0.1:5173...");
  await page.goto('http://127.0.0.1:5173');
  
  // Wait for the app shell to mount
  await page.waitForSelector('.app-shell', { timeout: 10000 });
  await page.waitForTimeout(2000); // let animations settle
  
  // Take screenshot of the dark theme (default: Target)
  const panel = page.locator('.right-pane-bridge');
  await page.waitForTimeout(1000);
  console.log("Taking dark theme screenshot (Target)...");
  await panel.screenshot({ path: '/home/camer/pigide/target_dark_screenshot.png' });
  
  // Open Theme Picker
  console.log("Opening theme picker...");
  await page.click('button[title^="Theme:"]');
  await page.waitForSelector('.theme-picker', { timeout: 5000 });
  
  // Click on Atom Light theme
  console.log("Selecting Atom Light theme...");
  await page.click('button[aria-label="Atom Light (light)"]');
  await page.waitForTimeout(1000); // let theme transition complete
  
  // Close theme picker
  console.log("Closing theme picker...");
  await page.click('.theme-picker-close');
  await page.waitForTimeout(1000); // let UI settle
  
  // Take screenshot of the light theme (Atom Light)
  console.log("Taking light theme screenshot (Atom Light)...");
  await panel.screenshot({ path: '/home/camer/pigide/target_light_screenshot.png' });
});
