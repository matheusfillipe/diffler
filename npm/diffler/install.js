"use strict";

// The launcher fetches the binary on first run, so a failed prefetch here is fine.

const { ensureBinary } = require("./lib/resolve.js");

ensureBinary().catch((err) => {
  process.stderr.write(
    `diffler: could not prefetch the binary (${err.message}); ` +
      "run diffler to fetch it.\n",
  );
});
