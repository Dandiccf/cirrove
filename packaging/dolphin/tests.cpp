#include "cirroveclient.h"

#include <QJsonArray>
#include <QJsonObject>
#include <cstdlib>
#include <iostream>

using namespace CirroveDolphin;

namespace {
void require(bool condition, const char *message) {
  if (!condition) {
    std::cerr << message << '\n';
    std::exit(1);
  }
}
} // namespace

int main() {
  const QJsonObject status{
      {QStringLiteral("accounts"),
       QJsonArray{
           QJsonObject{
               {QStringLiteral("account_id"), QStringLiteral("one")},
               {QStringLiteral("label"), QStringLiteral("Personal")},
               {QStringLiteral("mounted"), true},
               {QStringLiteral("mount_path"), QStringLiteral("/cloud")}},
           QJsonObject{
               {QStringLiteral("account_id"), QStringLiteral("two")},
               {QStringLiteral("label"), QStringLiteral("Nested")},
               {QStringLiteral("mounted"), true},
               {QStringLiteral("mount_path"), QStringLiteral("/cloud/team")}},
           QJsonObject{{QStringLiteral("label"), QStringLiteral("Off")},
                       {QStringLiteral("mounted"), false},
                       {QStringLiteral("mount_path"), QStringLiteral("/off")}},
       }}};
  const auto mounts = mountsFromStatus(status);
  require(mounts.size() == 2, "only mounted accounts are eligible");
  const auto nested = locate(mounts, QStringLiteral("/cloud/team/a.txt"));
  require(nested && nested->label == QLatin1String("Nested") &&
              nested->relative == QLatin1String("a.txt"),
          "the longest mount must win");
  require(!locate(mounts, QStringLiteral("/cloudy/a.txt")),
          "a textual prefix is not a path below the mount");
  const auto root = locate(mounts, QStringLiteral("/cloud"));
  require(root && root->relative.isEmpty(),
          "the mount root has an empty relative path");

  PathState fetched{QStringLiteral("a"),
                    QStringLiteral("file"),
                    QStringLiteral("direct"),
                    10,
                    10,
                    {}};
  PathState fetching{QStringLiteral("b"),
                     QStringLiteral("file"),
                     QStringLiteral("direct"),
                     10,
                     4,
                     {}};
  PathState onDemand{
      QStringLiteral("c"), QStringLiteral("file"), {}, 10, 10, {}};
  require(overlayFor(fetched).endsWith(QLatin1String("-kept")),
          "a resident pin gets the kept badge");
  require(overlayFor(fetching).endsWith(QLatin1String("-fetching")),
          "an incomplete pin gets the fetching badge");
  require(overlayFor(onDemand).isEmpty(),
          "ordinary on-demand files get no badge");
  require(shouldUnpin({fetched, fetching}), "all direct pins offer unpin");
  require(!shouldUnpin({fetched, onDemand}),
          "a mixed selection never removes a pin");

  const auto body =
      pinBody(QStringLiteral("Drive"), QStringLiteral("folder"), true);
  require(body.value(QStringLiteral("label")) == QLatin1String("Drive") &&
              body.value(QStringLiteral("path")) == QLatin1String("folder") &&
              body.value(QStringLiteral("recursive")).toBool(),
          "folder actions preserve account, path and recursion");
  return 0;
}
