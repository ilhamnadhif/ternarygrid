# Homebrew cask for TernaryGrid.
# Publish the notarized DMG produced by scripts/bundle-macos.sh to the release page,
# then update `version` and `sha256` (printed by the script). Install it with:
#   brew install --cask ./packaging/homebrew/ternarygrid.rb
cask "ternarygrid" do
  version "0.7.0"
  sha256 "713e05cc8381e0809a25b3f8508035a267b47b51d38d11f4d744ecf1603e224c"

  url "https://github.com/ilhamnadhif/ternarygrid/releases/download/v#{version}/TernaryGrid-#{version}.dmg"
  name "TernaryGrid"
  desc "Lightweight SQL database client written in Rust"
  homepage "https://github.com/ilhamnadhif/ternarygrid"

  depends_on macos: ">= :monterey"

  app "TernaryGrid.app"
  binary "#{appdir}/TernaryGrid.app/Contents/MacOS/ternarygrid"

  zap trash: [
    "~/Library/Application Support/dev.ternarygrid.TernaryGrid",
    "~/Library/Caches/dev.ternarygrid.TernaryGrid",
    "~/Library/Preferences/dev.ternarygrid.TernaryGrid.plist",
  ]
end
