#include <KOverlayIconPlugin>
#include <KPluginFactory>
#include <KPluginMetaData>
#include <QCoreApplication>
#include <QPluginLoader>
#include <cstdlib>
#include <iostream>

namespace {
void require(bool condition, const char *message) {
  if (!condition) {
    std::cerr << message << '\n';
    std::exit(1);
  }
}
} // namespace

int main(int argc, char **argv) {
  QCoreApplication application(argc, argv);
  require(argc == 3, "expected the action and overlay plugin paths");

  QPluginLoader actionLoader(QString::fromLocal8Bit(argv[1]));
  KPluginMetaData actionMetadata(actionLoader);
  require(actionMetadata.isValid(),
          "the action plugin has no valid KDE metadata");
  require(actionMetadata.pluginId() == QLatin1String("cirrovefileitemaction"),
          "the action plugin id changed");
  require(
      actionMetadata.supportsMimeType(QStringLiteral("text/plain")) &&
          actionMetadata.supportsMimeType(QStringLiteral("image/png")) &&
          actionMetadata.supportsMimeType(QStringLiteral("inode/directory")),
      "the action is not discoverable for ordinary files and folders");
  require(qobject_cast<KPluginFactory *>(actionLoader.instance()),
          "the action module does not export a KPluginFactory");

  QPluginLoader overlayLoader(QString::fromLocal8Bit(argv[2]));
  require(qobject_cast<KOverlayIconPlugin *>(overlayLoader.instance()),
          "the overlay module does not export KOverlayIconPlugin");
  return 0;
}
