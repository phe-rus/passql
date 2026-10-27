// Load the native passql binding for the current platform.
// In a published package, @napi-rs/cli's optionalDependencies would
// pull in the right platform-specific binary automatically. For local
// development we just load the file from the current directory.

const path = require('path');
const fs = require('fs');

function loadNative() {
  // Look for a passql.<platform>.node file in the current directory.
  const files = fs.readdirSync(__dirname);
  const nativeFile = files.find((f) => /^passql\..*\.node$/.test(f));
  if (!nativeFile) {
    throw new Error(
      `passql: no native binding found in ${__dirname}. ` +
      `Run 'npm run build' to compile the binding for your platform.`,
    );
  }
  return require(path.join(__dirname, nativeFile));
}

module.exports = loadNative();
