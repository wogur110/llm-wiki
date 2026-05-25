/** @type {import('jest').Config} */
export default {
  testEnvironment: "node",
  // No transform — let Node handle ESM natively via --experimental-vm-modules
  transform: {},
  // Only run tests under scripts/__tests__/
  testMatch: ["**/scripts/__tests__/**/*.test.js"],
};
