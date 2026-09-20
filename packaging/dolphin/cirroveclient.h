#pragma once

#include <QJsonObject>
#include <QList>
#include <QObject>
#include <QString>
#include <QStringList>
#include <functional>
#include <optional>

namespace CirroveDolphin {

struct Mount {
  QString accountId;
  QString label;
  QString path;
};

struct LocatedPath {
  QString label;
  QString relative;
};

struct PathState {
  QString path;
  QString kind;
  QString pinned;
  qint64 size = 0;
  qint64 resident = 0;
  QString refusal;
};

QString controlSocketPath();
QList<Mount> mountsFromStatus(const QJsonObject &status);
std::optional<LocatedPath> locate(const QList<Mount> &mounts,
                                  const QString &path);
QList<PathState> statesFromReply(const QJsonObject &reply);
QString overlayFor(const PathState &state);
bool shouldUnpin(const QList<PathState> &states);
QJsonObject pinBody(const QString &label, const QString &relative,
                    bool recursive);

std::optional<QJsonObject> requestSync(const QString &verb,
                                       const QJsonObject &body = {},
                                       int timeoutMs = 500);

using ReplyHandler = std::function<void(std::optional<QJsonObject>)>;
void requestAsync(QObject *owner, const QString &verb, const QJsonObject &body,
                  ReplyHandler handler, int timeoutMs = 3000);

} // namespace CirroveDolphin
