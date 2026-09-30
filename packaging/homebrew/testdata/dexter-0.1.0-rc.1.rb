cask "dexter" do
  version "0.1.0-rc.1"
  sha256 "7a9dbe24fd21d3a5d285b09448a2cb663fcd10c5a7e0d8b13823c952ae173cde"

  url "https://github.com/Shugar03/dexter/releases/download/v#{version}/dexter-#{version}-macos-universal.tar.gz"
  name "dexter"
  desc "Local-first agent computer runtime for macOS"
  homepage "https://github.com/Shugar03/dexter"

  depends_on macos: :ventura

  binary "dexter"
  binary "dexter-overlay"

  caveats <<~EOS
    dexter is ad-hoc signed and not notarized. Gatekeeper blocks
    the first run — clear quarantine once:
      xattr -d com.apple.quarantine $(which dexter)
    It also needs Accessibility and Screen Recording permission —
    run `dexter doctor --request` to check and request them.
  EOS
end
