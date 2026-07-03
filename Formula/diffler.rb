class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.3.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.3.0/diffler-v0.3.0-aarch64-apple-darwin.tar.gz"
      sha256 "78c69f95594fd0e5d04461139079a1f0bd38b9723344211248b545c25843f0ad"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.3.0/diffler-v0.3.0-x86_64-apple-darwin.tar.gz"
      sha256 "d512f3bac96431a39b652468b5e8c3501a960e316751f5264552b33a90267327"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.3.0/diffler-v0.3.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "45caded5edcfbb0e5e60868b53722eee15b356bdc8f5c995456e67e95cc5587d"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.3.0/diffler-v0.3.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "834520be66e9f166eaf3aaef93dc3e7ac0551e1e0fa9e5cdda6cd35c4008aae6"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
