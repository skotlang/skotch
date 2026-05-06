; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [3 x i8] c"Q1\00", align 1
@.str.1 = private unnamed_addr constant [3 x i8] c"Q2\00", align 1
@.str.2 = private unnamed_addr constant [3 x i8] c"Q3\00", align 1
@.str.3 = private unnamed_addr constant [3 x i8] c"Q4\00", align 1
@.str.4 = private unnamed_addr constant [5 x i8] c"axis\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_classify(i32 %arg0, i32 %arg1) {
entry:
  %merge_3 = alloca ptr
  %merge_4 = alloca i32
  %merge_10 = alloca i32
  %merge_16 = alloca i32
  %merge_22 = alloca i32
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 0
  %t2 = icmp sgt i32 %arg0, %t1
  store i32 0, ptr %merge_4
  br i1 %t2, label %bb11, label %bb12
bb2:
  store ptr @.str.0, ptr %merge_3
  br label %bb10
bb3:
  %t3 = add i32 0, 0
  %t4 = icmp slt i32 %arg0, %t3
  store i32 0, ptr %merge_10
  br i1 %t4, label %bb13, label %bb14
bb4:
  store ptr @.str.1, ptr %merge_3
  br label %bb10
bb5:
  %t5 = add i32 0, 0
  %t6 = icmp slt i32 %arg0, %t5
  store i32 0, ptr %merge_16
  br i1 %t6, label %bb15, label %bb16
bb6:
  store ptr @.str.2, ptr %merge_3
  br label %bb10
bb7:
  %t7 = add i32 0, 0
  %t8 = icmp sgt i32 %arg0, %t7
  store i32 0, ptr %merge_22
  br i1 %t8, label %bb17, label %bb18
bb8:
  store ptr @.str.3, ptr %merge_3
  br label %bb10
bb9:
  store ptr @.str.4, ptr %merge_3
  br label %bb10
bb10:
  %t9 = load ptr, ptr %merge_3
  ret ptr %t9
bb11:
  %t10 = add i32 0, 0
  %t11 = icmp sgt i32 %arg1, %t10
  %t12 = zext i1 %t11 to i32
  store i32 %t12, ptr %merge_4
  br label %bb12
bb12:
  %t13 = trunc i32 null to i1
  br i1 %t13, label %bb2, label %bb3
bb13:
  %t14 = add i32 0, 0
  %t15 = icmp sgt i32 %arg1, %t14
  %t16 = zext i1 %t15 to i32
  store i32 %t16, ptr %merge_10
  br label %bb14
bb14:
  %t17 = trunc i32 null to i1
  br i1 %t17, label %bb4, label %bb5
bb15:
  %t18 = add i32 0, 0
  %t19 = icmp slt i32 %arg1, %t18
  %t20 = zext i1 %t19 to i32
  store i32 %t20, ptr %merge_16
  br label %bb16
bb16:
  %t21 = trunc i32 null to i1
  br i1 %t21, label %bb6, label %bb7
bb17:
  %t22 = add i32 0, 0
  %t23 = icmp slt i32 %arg1, %t22
  %t24 = zext i1 %t23 to i32
  store i32 %t24, ptr %merge_22
  br label %bb18
bb18:
  %t25 = trunc i32 null to i1
  br i1 %t25, label %bb8, label %bb9
}

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  %t1 = add i32 0, 1
  %t2 = call ptr @InputKt_classify(i32 %t0, i32 %t1)
  call i32 @puts(ptr %t2)
  %t4 = add i32 0, -1
  %t5 = add i32 0, 1
  %t6 = call ptr @InputKt_classify(i32 %t4, i32 %t5)
  call i32 @puts(ptr %t6)
  %t8 = add i32 0, -1
  %t9 = add i32 0, -1
  %t10 = call ptr @InputKt_classify(i32 %t8, i32 %t9)
  call i32 @puts(ptr %t10)
  %t12 = add i32 0, 1
  %t13 = add i32 0, -1
  %t14 = call ptr @InputKt_classify(i32 %t12, i32 %t13)
  call i32 @puts(ptr %t14)
  %t16 = add i32 0, 0
  %t17 = add i32 0, 5
  %t18 = call ptr @InputKt_classify(i32 %t16, i32 %t17)
  call i32 @puts(ptr %t18)
  ret i32 0
}

