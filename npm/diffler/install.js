"use strict";

// postinstall: fetch the platform binary up front. A failure here is not fatal
// — the launcher shim downloads it lazily on first run, so `npm install
// --ignore-scripts` and offline-then-online still work.

const { ensureBinary } = require("./lib/resolve.js");

ensureBinary().catch((err) => {
  process.stderr.write(
    `diffler: could not prefetch the binary (${err.message}); ` +
      "it will be fetched on first run.\n",
  );
});
