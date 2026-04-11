; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [5 x i8] c"zero\00", align 1
@.str.1 = private unnamed_addr constant [4 x i8] c"one\00", align 1
@.str.2 = private unnamed_addr constant [5 x i8] c"many\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_describe(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  br label %bb1
bb1:
  %t0 = add i32 0, 0
  %t1 = icmp eq i32 %arg0, %t0
  br i1 %t1, label %bb2, label %bb3
bb2:
  store ptr @.str.0, ptr %merge_2
  br label %bb6
bb3:
  %t2 = add i32 0, 1
  %t3 = icmp eq i32 %arg0, %t2
  br i1 %t3, label %bb4, label %bb5
bb4:
  store ptr @.str.1, ptr %merge_2
  br label %bb6
bb5:
  store ptr @.str.2, ptr %merge_2
  br label %bb6
bb6:
  %t4 = load ptr, ptr %merge_2
  ret ptr %t4
}

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  %t1 = call ptr @InputKt_describe(i32 %t0)
  call i32 @puts(ptr %t1)
  ret i32 0
}

