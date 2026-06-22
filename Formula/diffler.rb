class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.1/diffler-v0.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "163bc3045fff2a71a51d00510fc6d53d0e9199d7335e8d159a69a55d513c3793"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.1/diffler-v0.2.1-x86_64-apple-darwin.tar.gz"
      sha256 "c1d71ef495028da7993d4705f7542b2eaff7212ee92ecdf9dea6a79303758d5c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.1/diffler-v0.2.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "377f076ffd95c2b3ee4d56bafb7c9226ad9c54970b4de7484fb2d3d965e450bd"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.1/diffler-v0.2.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "02ef981daf93efe77219f79abd9941dd5990551eb9b614929826af7c09276227"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
