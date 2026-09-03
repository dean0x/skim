#!/bin/sh
# Zero-function deploy script: top-level commands and variable assignments only.
# Structure mode must render this meaningfully (not near-empty).

APP_NAME=myapp
REGISTRY=registry.example.com
TAG=${1:-latest}
NAMESPACE=${NAMESPACE:-production}

echo "Deploying $APP_NAME:$TAG to $NAMESPACE"

docker pull "$REGISTRY/$APP_NAME:$TAG"
docker tag "$REGISTRY/$APP_NAME:$TAG" "$APP_NAME:current"

kubectl apply -f k8s/deployment.yaml
kubectl set image deployment/$APP_NAME $APP_NAME="$REGISTRY/$APP_NAME:$TAG" -n "$NAMESPACE"
kubectl rollout status deployment/$APP_NAME -n "$NAMESPACE"

echo "Done"
