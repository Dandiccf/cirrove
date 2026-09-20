#include "cirroveclient.h"

#include <QDir>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLocalSocket>
#include <QPointer>
#include <QTimer>
#include <algorithm>
#include <unistd.h>
#include <utility>

namespace CirroveDolphin {
namespace {

QByteArray requestLine(const QString &verb, const QJsonObject &body) {
  QByteArray line = verb.toUtf8();
  if (!body.isEmpty()) {
    line += ' ';
    line += QJsonDocument(body).toJson(QJsonDocument::Compact);
  }
  line += '\n';
  return line;
}

std::optional<QJsonObject> decodeReply(const QByteArray &bytes) {
  QJsonParseError error;
  const auto document = QJsonDocument::fromJson(bytes, &error);
  if (error.error != QJsonParseError::NoError || !document.isObject()) {
    return std::nullopt;
  }
  return document.object();
}

class AsyncRequest final : public QObject {
public:
  AsyncRequest(QObject *owner, const QString &verb, const QJsonObject &body,
               ReplyHandler handler, int timeoutMs)
      : QObject(owner), handler_(std::move(handler)) {
    timer_.setSingleShot(true);
    timer_.setInterval(timeoutMs);
    connect(&timer_, &QTimer::timeout, this, [this] { finish(std::nullopt); });
    connect(&socket_, &QLocalSocket::connected, this, [this, verb, body] {
      socket_.write(requestLine(verb, body));
      socket_.flush();
    });
    connect(&socket_, &QLocalSocket::readyRead, this,
            [this] { received_ += socket_.readAll(); });
    connect(&socket_, &QLocalSocket::disconnected, this, [this] {
      received_ += socket_.readAll();
      finish(decodeReply(received_));
    });
    connect(&socket_, &QLocalSocket::errorOccurred, this,
            [this](QLocalSocket::LocalSocketError) { finish(std::nullopt); });
    timer_.start();
    socket_.connectToServer(controlSocketPath(), QIODevice::ReadWrite);
  }

private:
  void finish(std::optional<QJsonObject> reply) {
    if (finished_) {
      return;
    }
    finished_ = true;
    timer_.stop();
    socket_.abort();
    auto handler = std::move(handler_);
    deleteLater();
    handler(std::move(reply));
  }

  QLocalSocket socket_;
  QTimer timer_;
  QByteArray received_;
  ReplyHandler handler_;
  bool finished_ = false;
};

QString cleanAbsolute(const QString &path) {
  return QDir::cleanPath(QFileInfo(path).absoluteFilePath());
}

} // namespace

QString controlSocketPath() {
  const auto runtime = qEnvironmentVariable(
      "XDG_RUNTIME_DIR", QStringLiteral("/run/user/%1").arg(getuid()));
  return QDir(runtime).filePath(QStringLiteral("cirrove/control.sock"));
}

QList<Mount> mountsFromStatus(const QJsonObject &status) {
  QList<Mount> result;
  for (const auto value : status.value(QStringLiteral("accounts")).toArray()) {
    const auto account = value.toObject();
    if (!account.value(QStringLiteral("mounted")).toBool()) {
      continue;
    }
    const auto path = account.value(QStringLiteral("mount_path")).toString();
    const auto label = account.value(QStringLiteral("label")).toString();
    if (!path.isEmpty() && !label.isEmpty()) {
      result.push_back({account.value(QStringLiteral("account_id")).toString(),
                        label, cleanAbsolute(path)});
    }
  }
  return result;
}

std::optional<LocatedPath> locate(const QList<Mount> &mounts,
                                  const QString &path) {
  const auto candidate = cleanAbsolute(path);
  const Mount *best = nullptr;
  QString relative;
  for (const auto &mount : mounts) {
    const auto root = QDir::cleanPath(mount.path);
    QString current;
    if (candidate == root) {
      current = QString();
    } else if (candidate.startsWith(root + QLatin1Char('/'))) {
      current = candidate.mid(root.size() + 1);
    } else {
      continue;
    }
    if (!best || root.size() > best->path.size()) {
      best = &mount;
      relative = current;
    }
  }
  if (!best) {
    return std::nullopt;
  }
  return LocatedPath{best->label, relative};
}

QList<PathState> statesFromReply(const QJsonObject &reply) {
  QList<PathState> result;
  if (!reply.value(QStringLiteral("refusal")).toString().isEmpty()) {
    return result;
  }
  for (const auto value : reply.value(QStringLiteral("states")).toArray()) {
    const auto state = value.toObject();
    result.push_back({state.value(QStringLiteral("path")).toString(),
                      state.value(QStringLiteral("kind")).toString(),
                      state.value(QStringLiteral("pinned")).toString(),
                      state.value(QStringLiteral("size")).toInteger(),
                      state.value(QStringLiteral("resident")).toInteger(),
                      state.value(QStringLiteral("refusal")).toString()});
  }
  return result;
}

QString overlayFor(const PathState &state) {
  if (!state.refusal.isEmpty() || state.pinned.isEmpty()) {
    return {};
  }
  if (state.kind == QLatin1String("folder") || state.resident >= state.size) {
    return QStringLiteral("io.github.Dandiccf.Cirrove-kept");
  }
  return QStringLiteral("io.github.Dandiccf.Cirrove-fetching");
}

bool shouldUnpin(const QList<PathState> &states) {
  return !states.isEmpty() &&
         std::all_of(states.cbegin(), states.cend(),
                     [](const PathState &state) {
                       return state.refusal.isEmpty() &&
                              state.pinned == QLatin1String("direct");
                     });
}

QJsonObject pinBody(const QString &label, const QString &relative,
                    bool recursive) {
  return {{QStringLiteral("label"), label},
          {QStringLiteral("path"), relative},
          {QStringLiteral("recursive"), recursive}};
}

std::optional<QJsonObject> requestSync(const QString &verb,
                                       const QJsonObject &body, int timeoutMs) {
  QLocalSocket socket;
  socket.connectToServer(controlSocketPath(), QIODevice::ReadWrite);
  if (!socket.waitForConnected(timeoutMs)) {
    return std::nullopt;
  }
  const auto line = requestLine(verb, body);
  if (socket.write(line) != line.size() ||
      !socket.waitForBytesWritten(timeoutMs)) {
    return std::nullopt;
  }
  QByteArray received;
  QTimer deadline;
  deadline.setSingleShot(true);
  deadline.start(timeoutMs);
  while (deadline.remainingTime() > 0) {
    if (socket.bytesAvailable() == 0 &&
        !socket.waitForReadyRead(std::max(1, deadline.remainingTime()))) {
      if (socket.state() == QLocalSocket::UnconnectedState) {
        break;
      }
      continue;
    }
    received += socket.readAll();
  }
  received += socket.readAll();
  return decodeReply(received);
}

void requestAsync(QObject *owner, const QString &verb, const QJsonObject &body,
                  ReplyHandler handler, int timeoutMs) {
  new AsyncRequest(owner, verb, body, std::move(handler), timeoutMs);
}

} // namespace CirroveDolphin
