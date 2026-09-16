class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.14.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.0/diffler-v0.14.0-aarch64-apple-darwin.tar.gz"
      sha256 "84774fc6b6e2fbb3db2b48b5e3d81bb686f71bca400dd5aa6d08fc5938b40f93"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.0/diffler-v0.14.0-x86_64-apple-darwin.tar.gz"
      sha256 "4cc767aa95c5c453f64be0dba3baaaae73e86e4cc8066b75664e183c4291c2f2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.0/diffler-v0.14.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "cfc3836528a20f2e70330a542bf9a9b7ca8c1e36465ac65383d8726422dfb801"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.0/diffler-v0.14.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d2a9a9f69943ded2edad5115939da1682ff06e7c726ec9ce328dab572d5220cf"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
